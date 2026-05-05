# a5px Rust audit

Scope: src/rust/src/ — read.rs, sample.rs, band_fetch.rs, parquet_write.rs,
geo.rs, cell_raw.rs, error.rs, lib.rs. No source files were edited.

## 1. Concurrency in the producer/consumer pipeline

### [IMPORTANT] Detached consumer tasks on early return — read.rs:773, 779-784

When `producer.await?` returns Err, the `?` exits `read_raster_async`
before the consumer-join loop runs. The consumer `JoinHandle`s are
dropped without `.await`; tokio detaches rather than aborts. The
detached consumers continue running on the blocking pool (decoding
already-queued tiles, building per-worker maps that no one reads). They
exit cleanly because the channel closes (all senders drop during
unwind), but they hold their `Arc`s and run to completion after the R
call has returned. Same pattern in sample.rs:345, 353-357.

This is not a deadlock and not unsoundness, but the first consumer
error gets shadowed if the producer also fails, and CPU keeps burning
after R sees the error.

Fix sketch: on producer error, `for h in consumer_handles { h.abort(); }`
before propagating, OR collect both the producer result and consumer
results before returning so consumer errors aren't silently lost.

### [IMPORTANT] First consumer error wins, others detached — read.rs:779-784

Symmetric to the above but for the success-of-producer path: if a
consumer returns `Err`, the `for h in consumer_handles { let m = h.await??; ... }`
loop short-circuits via `?` and the remaining handles are dropped. Those
remaining consumers keep running detached. Their later errors (if any)
are lost.

Fix sketch: collect all results into `Vec<Result<...>>` first, then
fold; or `try_join_all` over the JoinHandles (with the JoinError mapped).

### [NIT] JoinError mis-classified as Invalid — read.rs:782, sample.rs:356

`map_err(|e| A5CogError::Invalid(format!("consumer join: {e}")))` is
wrong: a `tokio::task::JoinError` indicates panic or cancellation, not
invalid user input. Misleading error class for the diagnosis.

### [NIT] Drop-order comment is now stale — read.rs:636-639, 720-722, 774

The comments still describe the previous deadlock fix. The current
sequence (spawn consumers → drop outer rx_chan → drive producer → drop
tx_chan) is correct: outer rx_chan is dropped at line 722 before any
producer await, and tx_chan is moved into the inner block (line 730)
so it closes when that block exits or unwinds. async-channel handles
the close-on-empty-receiver direction correctly, so a single consumer
returning Err with `cpu_workers=1` will cause the producer's `send`
to fail with SendError and the producer task to error out cleanly
rather than block. Worth reflowing the comment to capture that
because the current text suggests there's still ambiguity.

### [NIT] Second `producer` block has redundant final drop — read.rs:774

`drop(tx_chan)` at end of the block is a no-op: `tx_chan` is moved
in, and would be dropped at end-of-scope anyway. Harmless, but the
companion comment at line 771-772 implies it's load-bearing.

## 2. Soundness of the unsafe cell-cache fast-path

### no findings

The invariant required for `from_raw_parts_mut(last_entry_ptr, n_out)`
to be sound is:

1. `last_entry_ptr` was set to `v.as_mut_ptr()` of a `Vec<Accum>`
   inserted into `local` via `or_insert_with(|| vec![Accum::new(); n_out])`,
   AND
2. between the set and the next deref, no operation has dropped or
   reallocated the heap buffer that pointer addresses, AND
3. no aliasing `&mut` exists to that buffer.

Walking the loop body in process_tile (read.rs:364-454):

- The only `local` mutation is `local.entry(cell).or_insert_with(...)`
  in the else branch (line 438-440), which always resets last_entry_ptr
  on the same iteration to point at the just-inserted Vec.
- AHashMap rehash on insertion may MOVE the `Vec<Accum>` struct (3
  words) into a new bucket array, but the heap pointer stored in
  `Vec.ptr` is preserved by memcpy. Pointers obtained via
  `as_mut_ptr()` address the heap buffer, not the Vec struct, so
  they survive a rehash.
- `vec![Accum::new(); n_out]` allocates with cap == len == n_out, and
  no code path pushes/extends the Vec (only `entry[b].push(v)`, which
  is `Accum::push`, an in-place struct mutation, not `Vec::push`). So
  no realloc.
- The aliasing rule holds because the previous-iteration `&mut [Accum]`
  borrow has ended by the time the next iteration reborrows: the
  closure scope of each loop iteration ends at the iteration boundary.
- Inserts of OTHER cells are still permitted (they go through the
  else branch and reset last_entry_ptr, dropping the old borrow
  through the `last_entry_ptr` field — which is a raw pointer, so
  has no borrow). The unsafe reborrow on the next match-`last_cell`
  iteration is valid because `local` was not modified in the if
  branch that produced the cache hit.

The SAFETY comment at read.rs:431 is accurate.

## 3. GeoKey-based CRS reconstruction

### [BLOCKER] CT_ObliqueStereographic (16) emits +proj=stere — geo.rs:139-142

GeoTIFF coord_trans 14, 15, 16 are folded into `+proj=stere`. That's
correct for Stereographic and Polar Stereographic, but
CT_ObliqueStereographic (16) is the "double stereographic" used by
RD New / Amersfoort etc., and proj distinguishes it as `+proj=sterea`.
Files using CT 16 will silently produce coordinates off by tens of
metres because the conformal-sphere step is skipped.

Fix sketch: split the match arm — `15 => "stere"` (with optional
ProjStdParallel1 lat_ts handling, see below), `14 | 16 => "stere"`
or `14 => "stere", 16 => "sterea"`. Most concretely:

```
14 | 15 => ("stere", vec![format!("+k_0={k}")]),
16 => ("sterea", vec![format!("+k_0={k}")]),
```

### [IMPORTANT] Mercator 2SP path is wrong — geo.rs:107-110

CT_Mercator (7) covers both Mercator_1SP (uses
ProjScaleAtNatOriginGeoKey) and Mercator_2SP (uses
ProjStdParallel1GeoKey, with k implied to be cos(lat_ts)/sqrt(...)).
The code only reads `proj_scale_at_nat_origin`, defaulting to k=1, and
ignores `proj_std_parallel1`. A 2SP Mercator file produces a wrong
scale.

Fix sketch:

```
7 => {
    if let Some(p1) = geo.proj_std_parallel1 {
        ("merc", vec![format!("+lat_ts={p1}")])
    } else {
        let k = geo.proj_scale_at_nat_origin.unwrap_or(1.0);
        ("merc", vec![format!("+k_0={k}")])
    }
}
```

### [IMPORTANT] Polar Stereographic variant B ignored — geo.rs:139-142

CT_PolarStereographic (15) has two variants: variant A uses
ProjScaleAtNatOriginGeoKey; variant B uses ProjStdParallel1GeoKey
(`+lat_ts=`). The code only reads scale; variant-B files get k=1 and
no lat_ts, which is a real-world miss for products like NSIDC sea-ice
grids.

Fix sketch: as above for Mercator — prefer std_parallel1 → +lat_ts,
else fall back to k_0.

### [IMPORTANT] Cylindrical Equal Area missing +lat_ts — geo.rs:152

CT_CylindricalEqualArea (28) requires a standard parallel
(ProjStdParallel1GeoKey → `+lat_ts=`). The code emits no lat_ts, so
proj defaults to lat_ts=0 (equator). Files with a non-zero standard
parallel (Behrmann, Lambert cylindrical equal-area, Gall-Peters,
etc.) will produce a wrong scale.

Fix sketch: `28 => ("cea", geo.proj_std_parallel1.map(|p| format!("+lat_ts={p}")).into_iter().collect())`.

### [IMPORTANT] Equirectangular missing +lat_ts — geo.rs:144

Same shape as the previous: CT_Equirectangular (17) accepts
ProjStdParallel1GeoKey for the parallel of true scale; if present,
proj wants `+lat_ts=`. Currently silently dropped.

### [IMPORTANT] Prime meridian offset never applied — geo.rs:86-165

GeogPrimeMeridianGeoKey / GeogPrimeMeridianLongGeoKey are not read.
Files using a non-Greenwich meridian (Paris, Bogota, Madrid, Ferro)
will be off by a constant longitude. Rare in practice for COGs but
within the spec.

Fix sketch: append `+pm=<long_or_named>` to the proj string when the
geokey is present and non-zero.

### [IMPORTANT] Wrong ellipsoid mappings — geo.rs:194-208

Three of the eleven mappings are not equivalent to the EPSG ellipsoid
they're labelled as:

- `7003 => "andrae"` — EPSG 7003 is the Australian National Spheroid;
  proj's name is `aust_SA`. `andrae` is a different ellipsoid (Andrae
  1876). Wrong.
- `7034 => "clrk80"` — EPSG 7034 is "Clarke 1880" (the original);
  proj's `clrk80` is "Clarke 1880 (RGS)" which has slightly different
  axes. Use explicit `+a=6378249.145 +rf=293.465` or omit the
  shorthand. Mild but technically wrong.
- `7048 => "GRS80"` — EPSG 7048 is "GRS 1980 Authalic Sphere"
  (a sphere, R=6371007.181); GRS80 is the ellipsoid. These have
  different shapes; the sphere maps to `+ellps=sphere` (or
  `+R=6371007.181`), not GRS80. Wrong.

Fix sketch: change 7003 → "aust_SA". Drop 7034 (fall through to the
explicit a/b code path). Change 7048 → return None (caller falls
through to explicit a/b, which the GeoKey will provide if it set this
ellipsoid code).

### [NIT] Axis order not handled — geo.rs (function-level)

GeoTIFF / EPSG axis ordering for some projected CRS is
(Northing, Easting). proj4rs follows the GIS convention
(X=Easting, Y=Northing). The geotransform in the GeoTIFF was almost
certainly written by GDAL using GIS-axis convention regardless of the
EPSG declaration, so this matches proj4rs in practice. Mention only
for completeness; GDAL-written files are fine.

### [NIT] LAEA / AEA / others have no scale param — fine, but worth a comment

LAEA (10), AEA (11), AEQD (12), Sinusoidal (24) etc. don't take a
scale at all; the code's `extras: vec![]` is correct. A one-line
comment that "no scale param expected" would help future-you.

## 4. Error propagation

### [IMPORTANT] proj errors stringified via Debug — error.rs:35-39

`From<proj4rs::errors::Error>` formats via `{:?}` not `{}`. Some proj
errors carry path/parameter context in Display that gets lost. If
proj4rs implements Display, prefer it.

Fix sketch:
```
impl From<proj4rs::errors::Error> for A5CogError {
    fn from(e: proj4rs::errors::Error) -> Self { Self::Proj(e.to_string()) }
}
```

### [NIT] tokio runtime build error reported as Invalid — read.rs:1027, 1136, 1245, 1322; sample.rs (none — uses caller's runtime)

`map_err(|e| A5CogError::Invalid(format!("tokio runtime: {e}")))`. The
error class is wrong: this is a setup/internal failure, not user
input. Three call sites repeat this. Consider a new variant like
`Internal(String)` or just `Io(...)` if e.into() is acceptable.

### [NIT] Send-error message drops the underlying SendError — read.rs:763, sample.rs:337

`.map_err(|_| A5CogError::Invalid("consumer pool dropped channel".into()))`
is fine for a hot path, but obscures the real reason consumers
exited (panic vs. closed channel vs. user-cancelled). Low value;
flag only.

### [NIT] Several `Arrow*` errors collapsed to Invalid — parquet_write.rs:148, 160, 163, 166

`map_err(|e| A5CogError::Invalid(format!("...: {e}")))` is OK but
forces an `Invalid` taxonomy on every arrow/parquet failure. Could
add `Parquet(String)` and `Arrow(String)` variants if you want
better classification at the R layer.

### [NIT] gdal_nodata parse silently returns None — geo.rs:295-301

`lc.parse::<f64>().ok()` swallows the parse error. Documented in the
comment so this is intentional, but a malformed nodata tag silently
becomes "no nodata" rather than warning the user. Consider eprintln!
on failure when A5PX_PROFILE is set, or surface a dedicated warning
hook.

## 5. Idioms / ownership

### [NIT] parse_bbox_arg / parse_src_nodata_arg use Vec<f64> as Option sentinel — read.rs:833-862

This is the extendr-boundary contortion: NULL maps to empty Vec. The
shape is fine, but a thin wrapper (`fn arg_opt_f64(v: Vec<f64>, name: &str) -> Result<Option<f64>>`)
would centralise the length-validation logic and the empty-as-None
convention. Currently three near-duplicate parses for bbox + nodata.

### [NIT] Tokio runtime built per call — read.rs:1023, 1132, 1241, 1318

Each extendr entry point spins up a fresh multi-thread runtime,
processes one raster, and tears it down. Costs ~ms of setup. A
`OnceLock<tokio::runtime::Runtime>` lazily initialised in `.onLoad`-style
init would amortise it. Probably not worth the complexity given the
async work itself dominates.

### [NIT] `Arc<Vec<usize>>` for selected_bands and identity_offsets — read.rs:624-625

`Vec<usize>` is small (8 + 8 + n*8 bytes); `Arc<...>` is one
indirection per access in the hot loop (process_tile call). Could
pass slices directly into the consumer closure (clone the Vec into
each consumer at spawn time — N consumer clones is cheap). Wash
either way.

### [NIT] `for (i, a) in accs.iter().enumerate() { entry[i].merge(a); }` — read.rs:709-711, 790-792

`Accum: Copy`, so iterating by value `for (i, a) in accs.into_iter().enumerate()`
or even `for (entry_a, a) in entry.iter_mut().zip(accs) { entry_a.merge(&a); }`
reads cleaner and avoids index bounds checks. Trivial.

### [NIT] `file_metadata(...)` computed twice — parquet_write.rs:145, 151

Once for `Schema::with_metadata` (HashMap), once for KV (Vec). Bind
to a local and reuse:
```
let kv_pairs = file_metadata(...);
let schema = Schema::new(fields).with_metadata(kv_pairs.clone());
let kv: Vec<KeyValue> = kv_pairs.into_iter().map(|(k,v)| KeyValue::new(k,v)).collect();
```

### [NIT] `match` ladders that read better as `?` — read.rs:411-417, 420-426

```
match a5::lonlat_to_cell(lonlat, resolution) {
    Ok(id) => { ... id }
    Err(_) => continue,
}
```
twice, with the same body in both branches. Extract a small helper:
```
fn cell_at(ll: LonLat, res: i32, last: &mut Option<A5Cell>) -> Option<u64> { ... }
```
and the outer match collapses to a one-liner. Style only.

### [NIT] Sample.rs reduce loop iterates n_in × n_workers — sample.rs:353-365

For each consumer's flat/valid arrays, scans every input cell to
check the valid bit. Each cell touched by exactly one worker, so this
is O(n_in × n_workers) where O(n_in) total would suffice. With
typical small worker counts (≤16) it's fine; flag only.

### [NIT] `parse_src` schema list duplicated — read.rs:205-207

`["http://", "https://", "s3://", "gs://", "az://", "abfs://", "file://"]`
is hard-coded but `object_store::parse_url` already knows them all.
Could replace the manual prefix sniff with a `Url::parse(src).is_ok()`
+ scheme check. Minor.
