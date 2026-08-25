#![allow(clippy::panic, clippy::unwrap_used)]

//! Wire contract for the visualizer roster and the spectrum feed.

use spotuify_protocol::{
    canonical_viz_style, viz_style_is_known, viz_style_step, viz_style_uses_waveform, DaemonEvent,
    DEFAULT_VIZ_STYLE, VIZ_STYLES, VIZ_WAVEFORM_POINTS, VIZ_WAVEFORM_STYLES,
};

#[test]
fn every_roster_entry_is_unique_and_described() {
    let mut seen = std::collections::HashSet::new();
    for style in VIZ_STYLES {
        assert!(seen.insert(style.name), "duplicate style {}", style.name);
        assert!(
            style
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-'),
            "{} is not a kebab-case id",
            style.name
        );
        assert!(
            !style.description.is_empty(),
            "{} has no description",
            style.name
        );
        assert!(viz_style_is_known(style.name));
    }
    assert!(seen.contains(DEFAULT_VIZ_STYLE));
}

#[test]
fn cycling_visits_every_style_once_before_wrapping() {
    let mut style = DEFAULT_VIZ_STYLE;
    let mut visited = Vec::new();
    for _ in 0..VIZ_STYLES.len() {
        visited.push(style);
        style = viz_style_step(style, 1);
    }
    assert_eq!(style, DEFAULT_VIZ_STYLE, "cycle did not wrap to the start");
    assert_eq!(
        visited.len(),
        visited
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        "cycle repeats a style"
    );
}

#[test]
fn waveform_styles_are_named_in_the_roster() {
    for name in VIZ_WAVEFORM_STYLES {
        assert_eq!(
            canonical_viz_style(name),
            Some(name),
            "{name} is not a style"
        );
        assert!(viz_style_uses_waveform(name));
    }
    assert!(!viz_style_uses_waveform("bars"));
    assert!(!viz_style_uses_waveform("nope"));
    // Case and whitespace go through the same canonicaliser as everything else.
    assert!(viz_style_uses_waveform("  WAVE "));
}

/// A daemon older than the waveform field sends `spectrum-frame` without it.
/// Its absence must decode as "no waveform", not as a protocol error.
#[test]
fn a_spectrum_frame_without_a_waveform_still_decodes() {
    let json = r#"{"event":"spectrum-frame","bands":[0.1,0.2],"peak":0.5,"timestamp_ms":7}"#;
    let event: DaemonEvent = serde_json::from_str(json).expect("legacy frame rejected");
    let DaemonEvent::SpectrumFrame {
        bands,
        peak,
        timestamp_ms,
        waveform,
    } = event
    else {
        panic!("expected a spectrum frame");
    };
    assert_eq!(bands, vec![0.1, 0.2]);
    assert_eq!(peak, 0.5);
    assert_eq!(timestamp_ms, 7);
    assert!(waveform.is_empty());
}

/// An empty waveform is skipped on the wire, so spectrum styles do not pay
/// for a field nobody reads.
#[test]
fn an_empty_waveform_is_omitted_from_the_wire() {
    let event = DaemonEvent::SpectrumFrame {
        bands: vec![0.1],
        peak: 0.5,
        timestamp_ms: 7,
        waveform: Vec::new(),
    };
    let json = serde_json::to_string(&event).expect("encode");
    assert!(!json.contains("waveform"), "{json}");
}

#[test]
fn a_populated_waveform_round_trips() {
    let waveform: Vec<f32> = (0..VIZ_WAVEFORM_POINTS)
        .map(|i| (i as f32 / VIZ_WAVEFORM_POINTS as f32).mul_add(2.0, -1.0))
        .collect();
    let event = DaemonEvent::SpectrumFrame {
        bands: vec![0.0; 12],
        peak: 0.0,
        timestamp_ms: 1,
        waveform: waveform.clone(),
    };
    let json = serde_json::to_string(&event).expect("encode");
    assert!(json.contains("waveform"), "{json}");
    let decoded: DaemonEvent = serde_json::from_str(&json).expect("decode");
    assert_eq!(decoded, event);
    let DaemonEvent::SpectrumFrame { waveform: back, .. } = decoded else {
        panic!("expected a spectrum frame");
    };
    assert_eq!(back, waveform);
}
