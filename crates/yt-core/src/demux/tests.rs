//! Offline tests on synthetic boxes / elements.

use super::framing::*;
use super::*;

// ---- tiny MP4 writer -----------------------------------------------------------------------

fn bx(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(body);
    v
}

fn full(ty: &[u8; 4], ver: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = vec![ver];
    b.extend_from_slice(&flags.to_be_bytes()[1..]);
    b.extend_from_slice(body);
    bx(ty, &b)
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

const SPS: &[u8] = &[0x67, 0x64, 0x00, 0x28, 0xAC];
const PPS: &[u8] = &[0x68, 0xEE, 0x3C, 0x80];

fn avcc() -> Vec<u8> {
    let mut v = vec![1, 0x64, 0x00, 0x28, 0xFF, 0xE1];
    v.extend_from_slice(&(SPS.len() as u16).to_be_bytes());
    v.extend_from_slice(SPS);
    v.push(1);
    v.extend_from_slice(&(PPS.len() as u16).to_be_bytes());
    v.extend_from_slice(PPS);
    v
}

fn mp4_init(timescale: u32) -> Vec<u8> {
    let ftyp = bx(b"ftyp", b"dash\0\0\0\0iso6");
    let tkhd = full(b"tkhd", 0, 3, &cat(&[&[0; 8], &1u32.to_be_bytes(), &[0; 68]]));
    let mdhd = full(b"mdhd", 0, 0, &cat(&[&[0; 8], &timescale.to_be_bytes(), &[0; 8]]));
    let hdlr = full(b"hdlr", 0, 0, &cat(&[&[0; 4], b"vide", &[0; 13]]));
    let mut vse = vec![0u8; 78];
    vse[7] = 1; // data_reference_index
    vse[24..26].copy_from_slice(&1920u16.to_be_bytes());
    vse[26..28].copy_from_slice(&1080u16.to_be_bytes());
    let colr = bx(b"colr", &[b'n', b'c', b'l', b'x', 0, 9, 0, 16, 0, 9, 0]);
    let avc1 = bx(b"avc1", &cat(&[&vse, &bx(b"avcC", &avcc()), &colr]));
    let stsd = full(b"stsd", 0, 0, &cat(&[&1u32.to_be_bytes(), &avc1]));
    let stbl = bx(b"stbl", &stsd);
    let minf = bx(b"minf", &stbl);
    let mdia = bx(b"mdia", &cat(&[&mdhd, &hdlr, &minf]));
    let elst = full(b"elst", 0, 0, &cat(&[&1u32.to_be_bytes(), &0u32.to_be_bytes(), &3000u32.to_be_bytes(), &[0, 1, 0, 0]]));
    let edts = bx(b"edts", &elst);
    let trak = bx(b"trak", &cat(&[&tkhd, &edts, &mdia]));
    let trex = full(b"trex", 0, 0, &cat(&[&1u32.to_be_bytes(), &1u32.to_be_bytes(), &3000u32.to_be_bytes(), &0u32.to_be_bytes(), &0x0101_0000u32.to_be_bytes()]));
    let mvex = bx(b"mvex", &trex);
    let mvhd = full(b"mvhd", 0, 0, &cat(&[&[0; 8], &1000u32.to_be_bytes(), &10_000u32.to_be_bytes(), &[0; 80]]));
    let moov = bx(b"moov", &cat(&[&mvhd, &trak, &mvex]));
    cat(&[&ftyp, &moov])
}

/// moof+mdat with `samples` (size, cts) at decode time `tfdt`; first is a sync sample.
fn mp4_fragment(tfdt: u64, samples: &[Vec<u8>], cts: &[u32]) -> Vec<u8> {
    let mfhd = full(b"mfhd", 0, 0, &1u32.to_be_bytes());
    let tfhd = full(b"tfhd", 0, 0x02_0000, &1u32.to_be_bytes());
    let tfdt_b = full(b"tfdt", 1, 0, &tfdt.to_be_bytes());
    // trun: data_offset, first_sample_flags, size, cts.
    let build_trun = |data_off: u32| {
        let mut b = (samples.len() as u32).to_be_bytes().to_vec();
        b.extend_from_slice(&data_off.to_be_bytes());
        b.extend_from_slice(&0x0200_0000u32.to_be_bytes()); // first sample: depends on nothing, sync
        for (s, c) in samples.iter().zip(cts) {
            b.extend_from_slice(&(s.len() as u32).to_be_bytes());
            b.extend_from_slice(&c.to_be_bytes());
        }
        full(b"trun", 0, 0x1 | 0x4 | 0x200 | 0x800, &b)
    };
    let probe = bx(b"moof", &cat(&[&mfhd, &bx(b"traf", &cat(&[&tfhd, &tfdt_b, &build_trun(0)]))]));
    let data_off = probe.len() as u32 + 8;
    let moof = bx(b"moof", &cat(&[&mfhd, &bx(b"traf", &cat(&[&tfhd, &tfdt_b, &build_trun(data_off)]))]));
    let mdat = bx(b"mdat", &samples.concat());
    cat(&[&moof, &mdat])
}

fn nal_sample(nals: &[&[u8]]) -> Vec<u8> {
    let mut v = Vec::new();
    for n in nals {
        v.extend_from_slice(&(n.len() as u32).to_be_bytes());
        v.extend_from_slice(n);
    }
    v
}

#[test]
fn mp4_fragment_demux_incremental() {
    let init = mp4_init(90_000);
    let sidx = {
        let mut b = 1u32.to_be_bytes().to_vec();
        b.extend_from_slice(&90_000u32.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes()); // ept
        b.extend_from_slice(&0u32.to_be_bytes()); // first offset
        b.extend_from_slice(&[0, 0, 0, 2]);
        for (size, dur) in [(1000u32, 180_000u32), (2000, 90_000)] {
            b.extend_from_slice(&size.to_be_bytes());
            b.extend_from_slice(&dur.to_be_bytes());
            b.extend_from_slice(&0x9000_0000u32.to_be_bytes());
        }
        full(b"sidx", 0, 0, &b)
    };
    let s0 = nal_sample(&[&[0x65, 1, 2, 3]]);
    let s1 = nal_sample(&[&[0x41, 9]]);
    let s2 = nal_sample(&[&[0x01, 7, 7]]);
    let frag = mp4_fragment(3000, &[s0.clone(), s1.clone(), s2.clone()], &[6000, 12000, 3000]);
    let mut all = cat(&[&init, &sidx, &frag]);
    all.extend_from_slice(&bx(b"free", &[0; 100]));
    let mut d = mp4::Mp4Demuxer::new();
    for chunk in all.chunks(7) {
        d.push(chunk).unwrap();
    }
    let t = &d.tracks()[0];
    assert_eq!((t.codec.clone(), t.width, t.height, t.kind), (Codec::H264, 1920, 1080, Kind::Video));
    assert_eq!(t.color.as_ref().unwrap().transfer, 16);
    assert_eq!(t.config, avcc());
    let ix = d.index().unwrap();
    let sidx_end = (init.len() + sidx.len()) as u64;
    assert_eq!(ix[0], SegmentRef { start_ns: 0, duration_ns: 2_000_000_000, offset: sidx_end, size: 1000 });
    assert_eq!(ix[1].offset, sidx_end + 1000);
    assert_eq!(d.duration_ns(), Some(10_000_000_000));
    let aus: Vec<_> = std::iter::from_fn(|| d.pop()).collect();
    assert_eq!(aus.len(), 3);
    // tfdt 3000 + cts 6000 - elst 3000 = 6000 ticks @ 90 kHz.
    assert_eq!(aus[0].pts_ns, 66_666_666);
    assert_eq!(aus[0].dts_ns, 0);
    assert!(aus[0].keyframe && !aus[1].keyframe && !aus[2].keyframe);
    assert_eq!(aus[1].dts_ns, 33_333_333);
    assert_eq!(aus[2].pts_ns, (3000 + 6000 + 3000 - 3000) * 1_000_000_000 / 90_000);
    assert_eq!((aus[0].data.clone(), aus[1].data.clone(), aus[2].data.clone()), (s0, s1, s2));

    // Seek: reset to the fragment's offset and push it again.
    d.reset_to(sidx_end);
    d.push(&frag).unwrap();
    assert_eq!(std::iter::from_fn(|| d.pop()).count(), 3);

    // Framing: SPS/PPS land in front of the IDR only.
    let f = framing::Framer::new(&d.tracks()[0]).unwrap();
    let a = f.frame(&aus[0]);
    assert_eq!(a, cat(&[&[0, 0, 0, 1], SPS, &[0, 0, 0, 1], PPS, &[0, 0, 0, 1, 0x65, 1, 2, 3]]));
    assert_eq!(f.frame(&aus[1]), vec![0, 0, 0, 1, 0x41, 9]);
}

// ---- tiny EBML writer ----------------------------------------------------------------------

fn id_bytes(id: u32) -> Vec<u8> {
    let b = id.to_be_bytes();
    let skip = b.iter().position(|&x| x != 0).unwrap();
    b[skip..].to_vec()
}

fn el(id: u32, body: &[u8]) -> Vec<u8> {
    let mut v = id_bytes(id);
    // 8-byte size vint.
    let mut sz = (body.len() as u64).to_be_bytes();
    sz[0] = 0x01;
    v.extend_from_slice(&sz);
    v.extend_from_slice(body);
    v
}

fn el_unknown(id: u32) -> Vec<u8> {
    let mut v = id_bytes(id);
    v.push(0xFF);
    v
}

fn u(id: u32, v: u64) -> Vec<u8> {
    el(id, &v.to_be_bytes())
}

fn opus_head(ch: u8) -> Vec<u8> {
    let mut h = b"OpusHead".to_vec();
    h.extend_from_slice(&[1, ch, 0x38, 0x01, 0x80, 0xBB, 0, 0, 0, 0, 0]);
    h
}

fn block(track: u8, rel: i16, flags: u8, payload: &[u8]) -> Vec<u8> {
    let mut b = vec![0x80 | track];
    b.extend_from_slice(&rel.to_be_bytes());
    b.push(flags);
    b.extend_from_slice(payload);
    b
}

#[test]
fn webm_demux_blocks_cues_and_lacing() {
    let ebml = el(0x1A45_DFA3, &el(0x4282, b"webm"));
    let info = el(0x1549_A966, &cat(&[&u(0x2A_D7B1, 1_000_000), &el(0x4489, &5000f64.to_be_bytes())]));
    let colour = el(0x55B0, &cat(&[&u(0x55BA, 16), &u(0x55BB, 9), &u(0x55B1, 9)]));
    let video = el(0xAE, &cat(&[&u(0xD7, 1), &u(0x83, 1), &el(0x86, b"V_VP9"), &el(0xE0, &cat(&[&u(0xB0, 3840), &u(0xBA, 2160), &colour]))]));
    let audio = el(
        0xAE,
        &cat(&[&u(0xD7, 2), &u(0x83, 2), &el(0x86, b"A_OPUS"), &el(0x63A2, &opus_head(2)), &u(0x23_E383, 20_000_000), &el(0xE1, &cat(&[&el(0xB5, &48000f64.to_be_bytes()), &u(0x9F, 2)]))]),
    );
    let tracks = el(0x1654_AE6B, &cat(&[&video, &audio]));
    let cluster_body = cat(&[
        &u(0xE7, 1000),
        &el(0xA3, &block(1, 0, 0x80, &[0x82, 0x49, 0x83, 0x42, 0x00])),
        &el(0xA3, &block(2, 0, 0x80, b"opus0")),
        &el(0xA0, &cat(&[&el(0xA1, &block(1, 33, 0, b"inter")), &u(0xFB, 0xFFDF)])),
        // Xiph-laced: 3 Opus packets of 2, 3 and 1 bytes.
        &el(0xA3, &block(2, 40, 0x80 | 0x02, &[2, 2, 3, b'a', b'a', b'b', b'b', b'b', b'c'])),
        // EBML-laced: 3 packets of 2, 3 and 1 bytes (first size 2, delta +1 = 0xC0 in 1-byte signed vint).
        &el(0xA3, &block(2, 100, 0x80 | 0x06, &[2, 0x82, 0xC0, b'x', b'x', b'y', b'y', b'y', b'z'])),
    ]);
    // Segment data starts right after the Segment header; cues point at the cluster.
    let pre_cues = cat(&[&info, &tracks]);
    let cues_len = el(0x1C53_BB6B, &el(0xBB, &cat(&[&u(0xB3, 1000), &el(0xB7, &cat(&[&u(0xF7, 1), &u(0xF1, 0)]))]))).len();
    let cluster_pos = (pre_cues.len() + cues_len) as u64;
    let cues = el(0x1C53_BB6B, &el(0xBB, &cat(&[&u(0xB3, 1000), &el(0xB7, &cat(&[&u(0xF7, 1), &u(0xF1, cluster_pos)]))])));
    let cluster = el(0x1F43_B675, &cluster_body);
    let file = cat(&[&ebml, &el_unknown(0x1853_8067), &pre_cues, &cues, &cluster]);

    let mut d = mkv::MkvDemuxer::new();
    for chunk in file.chunks(5) {
        d.push(chunk).unwrap();
    }
    let t = d.tracks();
    assert_eq!(t.len(), 2);
    assert_eq!((t[0].codec.clone(), t[0].width, t[0].height), (Codec::Vp9, 3840, 2160));
    assert!(t[0].color.as_ref().unwrap().is_hdr());
    assert_eq!((t[1].codec.clone(), t[1].channels, t[1].sample_rate), (Codec::Opus, 2, 48000));
    assert_eq!(d.duration_ns(), Some(5_000_000_000));
    let seg_data = (ebml.len() + 5) as u64;
    assert_eq!(d.index().unwrap()[0].offset, seg_data + cluster_pos);
    assert_eq!(d.index().unwrap()[0].start_ns, 1_000_000_000);

    let aus: Vec<_> = std::iter::from_fn(|| d.pop()).collect();
    let summary: Vec<(u64, i64, bool, Vec<u8>)> = aus.iter().map(|a| (a.track, a.pts_ns / 1_000_000, a.keyframe, a.data.clone())).collect();
    assert_eq!(
        summary,
        vec![
            (1, 1000, true, vec![0x82, 0x49, 0x83, 0x42, 0x00]),
            (2, 1000, true, b"opus0".to_vec()),
            (1, 1033, false, b"inter".to_vec()),
            (2, 1040, true, b"aa".to_vec()),
            (2, 1060, true, b"bbb".to_vec()),
            (2, 1080, true, b"c".to_vec()),
            (2, 1100, true, b"xx".to_vec()),
            (2, 1120, true, b"yyy".to_vec()),
            (2, 1140, true, b"z".to_vec()),
        ]
    );
    // reset_to a cluster: blocks parse again.
    d.reset_to(seg_data + cluster_pos);
    d.push(&cluster).unwrap();
    assert_eq!(std::iter::from_fn(|| d.pop()).count(), 9);
}

#[test]
fn av1_temporal_units() {
    // av1C: marker/version 0x81, 3 fixed bytes, then a sequence header OBU (type 1, sized).
    let seq = [0x0A, 0x03, 0xAA, 0xBB, 0xCC];
    let av1c = cat(&[&[0x81, 0x00, 0x0C, 0x00], &seq]);
    assert_eq!(av1c_config_obus(&av1c), seq.to_vec());
    // A frame OBU (type 6) with size field.
    let frame = [0x32, 0x02, 0x11, 0x22];
    let mut out = Vec::new();
    av1_to_tu(&frame, true, &seq, &mut out);
    assert_eq!(out, cat(&[&AV1_TD, &seq, &frame]));
    out.clear();
    av1_to_tu(&frame, false, &seq, &mut out);
    assert_eq!(out, cat(&[&AV1_TD, &frame]));
    // A sizeless last OBU gets a size field.
    out.clear();
    av1_to_tu(&[0x30, 0x11, 0x22, 0x33], false, &seq, &mut out);
    assert_eq!(out, cat(&[&AV1_TD, &[0x32, 0x03, 0x11, 0x22, 0x33]]));
    // A keyframe that carries its own sequence header gets no second one.
    out.clear();
    let with_seq = cat(&[&seq, &frame]);
    av1_to_tu(&with_seq, true, &seq, &mut out);
    assert_eq!(out, cat(&[&AV1_TD, &with_seq]));
    // leb128 round trip.
    let mut v = Vec::new();
    write_leb128(300, &mut v);
    assert_eq!(read_leb128(&v), Some((300, 2)));
}

#[test]
fn vp9_superframe_split_and_keyframe() {
    let a = [0x82u8, 0x49, 0x83, 0x42, 0x00];
    let b = [0x86u8, 0x00, 0x40];
    // Index: marker 0b110_00_001 (2 frames, 1-byte sizes).
    let marker = 0xC1u8;
    let sf = cat(&[&a, &b, &[marker, a.len() as u8, b.len() as u8, marker]]);
    let parts = vp9_split_superframe(&sf);
    assert_eq!(parts, vec![&a[..], &b[..]]);
    assert_eq!(vp9_split_superframe(&a), vec![&a[..]]);
    assert!(vp9_is_keyframe(&a));
    assert!(!vp9_is_keyframe(&b));
}

#[test]
fn aac_adts_and_opus_head() {
    // AAC-LC 44.1 kHz stereo.
    let c = AacConfig::parse(&[0x12, 0x10]).unwrap();
    assert_eq!((c.object_type, c.sample_rate, c.channels), (2, 44100, 2));
    let h = c.adts_header(100);
    assert_eq!(h, [0xFF, 0xF1, 0x50, 0x80, 0x0D, 0x7F, 0xFC]);
    // HE-AAC explicit (aot 5, core 24 kHz): ADTS describes the LC core.
    let he = AacConfig::parse(&[0x2B, 0x11, 0x88, 0x00]).unwrap();
    assert_eq!((he.object_type, he.sample_rate, he.channels), (2, 24000, 2));
    // dOps → OpusHead.
    let dops = [0u8, 2, 0x01, 0x38, 0, 0, 0xBB, 0x80, 0, 0, 0];
    let head = opus_head_from_dops(&dops).unwrap();
    assert_eq!(head, opus_head(2));
    let p = OpusHead::parse(&head).unwrap();
    assert_eq!((p.channels, p.pre_skip, p.input_rate), (2, 312, 48000));
}

#[test]
fn container_sniff() {
    assert_eq!(Container::sniff(&[0x1A, 0x45, 0xDF, 0xA3, 0]), Some(Container::WebM));
    assert_eq!(Container::sniff(b"\0\0\0\x18ftypdash"), Some(Container::Mp4));
    assert_eq!(Container::from_mime("audio/webm; codecs=\"opus\""), Container::WebM);
}
