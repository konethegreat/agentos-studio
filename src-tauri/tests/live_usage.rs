//! Opt-in: `cargo test --test live_usage -- --ignored --nocapture`

use agentos_studio_lib::usage;

#[test]
#[ignore = "reads the real Hermes state.db"]
fn usage_reads_a_real_session() {
    let sessions = usage::recent_sessions_with_tokens(5);
    println!("sessions with tokens: {}", sessions.len());
    for id in &sessions {
        if let Some(u) = usage::usage(id).unwrap() {
            println!(
                "  {} model={:?} in={} out={} cost=${:.4} msgs={} compacted={} limit={:?} pct={:?}",
                id,
                u.model,
                u.input_tokens,
                u.output_tokens,
                u.cost_usd,
                u.messages,
                u.compacted_messages,
                u.context_limit,
                u.context_used_pct
            );
        }
    }
    let credits = usage::credits();
    for c in &credits {
        println!(
            "  {} {} remaining={:?} note={}",
            c.provider, c.kind, c.remaining_usd, c.note
        );
    }
    assert!(!credits.is_empty());
}
