use crate::proto::{CHUNK_HEADER, CHUNK_PAYLOAD, ChunkHeader, FLAG_KEYFRAME, FLAG_LAST, FLAG_PARITY};
use bytes::{BufMut, Bytes, BytesMut};

pub fn data_chunks(len: usize) -> usize {
    len.div_ceil(CHUNK_PAYLOAD).max(1)
}

pub fn parity_groups(data: usize) -> usize {
    data.div_ceil(4)
}

pub struct Frame {
    pub id: u32,
    pub timestamp_us: u64,
    pub data: Vec<Bytes>,
    pub parity: Vec<Bytes>,
    pub bytes: usize,
}

pub fn packetize(id: u32, timestamp_us: u64, image: &[u8], fec: bool) -> Option<Frame> {
    let total = data_chunks(image.len());
    if total > u16::MAX as usize {
        return None;
    }
    let chunk = |flags, idx, body: &[&[u8]]| {
        let size = body.iter().map(|b| b.len()).sum::<usize>();
        let mut out = BytesMut::with_capacity(CHUNK_HEADER + size);
        ChunkHeader {
            flags,
            frame_id: id,
            chunk_idx: idx as u16,
            chunk_total: total as u16,
            timestamp_us,
        }
        .write(&mut out);
        body.iter().for_each(|b| out.put_slice(b));
        out.freeze()
    };
    let pieces: Vec<&[u8]> = if image.is_empty() { vec![image] } else { image.chunks(CHUNK_PAYLOAD).collect() };
    let data: Vec<Bytes> = pieces
        .iter()
        .enumerate()
        .map(|(i, p)| chunk(FLAG_KEYFRAME | if i + 1 == total { FLAG_LAST } else { 0 }, i, &[p]))
        .collect();
    let parity = if fec {
        let groups = parity_groups(total);
        (0..groups)
            .map(|g| {
                let members: Vec<&[u8]> = pieces.iter().skip(g).step_by(groups).copied().collect();
                let (len_xor, xor) = xor_group(&members);
                chunk(FLAG_KEYFRAME | FLAG_PARITY, g, &[&len_xor.to_be_bytes(), &xor])
            })
            .collect()
    } else {
        Vec::new()
    };
    let bytes = data.iter().chain(&parity).map(Bytes::len).sum();
    Some(Frame { id, timestamp_us, data, parity, bytes })
}

pub fn xor_group(members: &[&[u8]]) -> (u16, Vec<u8>) {
    let longest = members.iter().map(|m| m.len()).max().unwrap_or(0);
    let mut xor = vec![0; longest];
    let mut len_xor = 0;
    for m in members {
        len_xor ^= m.len() as u16;
        xor.iter_mut().zip(*m).for_each(|(a, b)| *a ^= b);
    }
    (len_xor, xor)
}

pub fn repair(parity_payload: &[u8], known: &[&[u8]]) -> Option<Vec<u8>> {
    let (len_bytes, data_xor) = parity_payload.split_first_chunk::<2>()?;
    let (known_len, known_xor) = xor_group(known);
    let len = (u16::from_be_bytes(*len_bytes) ^ known_len) as usize;
    if len > data_xor.len() || known_xor.len() > data_xor.len() {
        return None;
    }
    let mut out = data_xor.to_vec();
    out.iter_mut().zip(&known_xor).for_each(|(a, b)| *a ^= b);
    out.truncate(len);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[test]
    fn chunk_counts() {
        assert_eq!(data_chunks(0), 1);
        assert_eq!(data_chunks(1200), 1);
        assert_eq!(data_chunks(1201), 2);
        assert_eq!(parity_groups(1), 1);
        assert_eq!(parity_groups(4), 1);
        assert_eq!(parity_groups(5), 2);
    }

    #[test]
    fn data_round_trip_and_flags() {
        let img = image(3700);
        let f = packetize(9, 77, &img, false).unwrap();
        assert_eq!(f.data.len(), 4);
        assert!(f.parity.is_empty());
        let mut joined = Vec::new();
        for (i, c) in f.data.iter().enumerate() {
            let h = ChunkHeader::parse(c).unwrap();
            assert_eq!((h.frame_id, h.chunk_idx as usize, h.chunk_total), (9, i, 4));
            assert_eq!(h.flags & FLAG_LAST != 0, i == 3);
            joined.extend_from_slice(&c[CHUNK_HEADER..]);
        }
        assert_eq!(joined, img);
    }

    #[test]
    fn every_single_loss_repairs_at_every_size() {
        for len in [1, 600, 1200, 1201, 4800, 4801, 6000, 9999, 24001] {
            let img = image(len);
            let f = packetize(1, 0, &img, true).unwrap();
            let groups = f.parity.len();
            for lost in 0..f.data.len() {
                let g = lost % groups;
                let known: Vec<&[u8]> = (g..f.data.len())
                    .step_by(groups)
                    .filter(|&i| i != lost)
                    .map(|i| &f.data[i][CHUNK_HEADER..])
                    .collect();
                let fixed = repair(&f.parity[g][CHUNK_HEADER..], &known).unwrap();
                assert_eq!(fixed, &f.data[lost][CHUNK_HEADER..], "len {len} lost {lost}");
            }
        }
    }

    #[test]
    fn burst_shorter_than_group_count_hits_each_group_once() {
        let f = packetize(1, 0, &image(1200 * 40), true).unwrap();
        let groups = f.parity.len();
        assert_eq!(groups, 10);
        for start in 0..f.data.len() - groups {
            let mut hit: Vec<_> = (start..start + groups).map(|i| i % groups).collect();
            hit.sort();
            hit.dedup();
            assert_eq!(hit.len(), groups);
        }
    }

    #[test]
    fn repair_rejects_short_parity() {
        assert!(repair(&[0], &[]).is_none());
        assert!(repair(&[0xff, 0xff, 1, 2], &[]).is_none());
    }

    #[test]
    fn rejects_oversized_frame() {
        assert!(packetize(1, 0, &vec![0; 1200 * 65536], false).is_none());
    }
}
