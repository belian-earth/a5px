use extendr_api::prelude::*;

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
