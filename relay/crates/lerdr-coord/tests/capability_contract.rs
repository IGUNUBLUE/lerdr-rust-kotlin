//! The `docs/03` §4.1 capability table is the contract: `CAPABILITIES`
//! must advertise exactly the names the table declares, no more and no
//! fewer. This guard exists because the list used to live only in code —
//! `pane_realtime_delta` shipped unadvertised for weeks precisely because
//! nothing compared the two.
//!
//! The test parses the markdown table between `### 4.1` and the next
//! `##` heading: every `` `backticked` `` name in the first column is a
//! declared capability. Names listed under "Defined but not advertised"
//! must stay out of the set.

use std::collections::BTreeSet;

use lerdr_core::protocol::CAPABILITIES;

const DOC: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../docs/03-protocol.md"
));

/// First-column capability names from the §4.1 table.
fn declared_capabilities() -> BTreeSet<String> {
    let section = DOC
        .split("### 4.1")
        .nth(1)
        .expect("docs/03 must carry the §4.1 capability contract");
    let section = section.split("\n## ").next().unwrap_or(section);
    section
        .lines()
        .filter(|line| line.starts_with('|'))
        .filter_map(|line| line.split('|').nth(1).map(str::trim).map(str::to_owned))
        .filter(|cell| !cell.is_empty() && !cell.starts_with("---") && cell != "Capability")
        .flat_map(|cell| {
            cell.split('`')
                .skip(1)
                .step_by(2)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn advertised_set_matches_the_declared_contract() {
    let declared = declared_capabilities();
    let advertised: BTreeSet<String> = CAPABILITIES.iter().map(|c| (*c).to_owned()).collect();
    assert_eq!(
        advertised, declared,
        "CAPABILITIES drifted from docs/03 §4.1 — update the table or the code"
    );
}

#[test]
fn deliberately_unadvertised_names_stay_out() {
    // `agent_response_copy` is documented absent — no clipboard backend.
    assert!(!CAPABILITIES.contains(&"agent_response_copy"));
}
