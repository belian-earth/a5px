use extendr_api::prelude::*;

/// NA sentinel used by a5R: top byte (b8) == 0xFC, which is an invalid
/// quintant in A5 so it can never collide with a real cell.
const NA_BYTE_B8: u8 = 0xFC;

/// Decode the b1..b8 raw-byte list (a5R's a5_cell internal format) into a
/// `Vec<u64>`. NA-valued cells are dropped (their indices into the input
/// are not preserved); use [`raw8_list_to_u64s_keep_na`] if you need
/// alignment.
pub(crate) fn raw8_list_to_u64s(list: &List) -> Vec<u64> {
    let names = ["b1", "b2", "b3", "b4", "b5", "b6", "b7", "b8"];
    // Pull out the raw byte slices. dollar() returns a temporary Robj wrapper;
    // the raw bytes themselves are owned by the input List and live long
    // enough for the conversion below.
    let mut buffers: [Vec<u8>; 8] = std::array::from_fn(|_| Vec::new());
    for (j, name) in names.iter().enumerate() {
        if let Ok(robj) = list.dollar(name) {
            if let Some(slice) = robj.as_raw_slice() {
                buffers[j] = slice.to_vec();
            }
        }
    }
    let n = buffers[0].len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        if buffers[7][i] == NA_BYTE_B8 {
            continue;
        }
        let bytes = [
            buffers[0][i], buffers[1][i], buffers[2][i], buffers[3][i],
            buffers[4][i], buffers[5][i], buffers[6][i], buffers[7][i],
        ];
        out.push(u64::from_le_bytes(bytes));
    }
    out
}

pub(crate) fn u64s_to_raw8_list(values: &[u64]) -> List {
    let n = values.len();
    let mut bufs: [Vec<u8>; 8] = std::array::from_fn(|_| vec![0u8; n]);
    for (i, v) in values.iter().enumerate() {
        let bytes = v.to_le_bytes();
        for (j, byte) in bytes.iter().enumerate() {
            bufs[j][i] = *byte;
        }
    }
    list!(
        b1 = Robj::from(bufs[0].as_slice()),
        b2 = Robj::from(bufs[1].as_slice()),
        b3 = Robj::from(bufs[2].as_slice()),
        b4 = Robj::from(bufs[3].as_slice()),
        b5 = Robj::from(bufs[4].as_slice()),
        b6 = Robj::from(bufs[5].as_slice()),
        b7 = Robj::from(bufs[6].as_slice()),
        b8 = Robj::from(bufs[7].as_slice())
    )
}
