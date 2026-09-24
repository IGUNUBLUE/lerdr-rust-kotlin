//! Phase-5 pane-content surface against the fake Herdr server: exact
//! method names, exact param serialization (required vs optional fields),
//! typed response decoding, refusal propagation, and the capability
//! ledger's observed notes.

mod support;

use serde_json::json;
use support::{Action, FakeHerdr};

use lerdr_herdr::{
    capabilities::features, Client, DispatchPhase, FeatureState, HerdrError, LayoutApplyParams,
    LayoutExportParams, LayoutNode, PaneCopyMotion, PaneCopyMotionParams, PaneCopySearchDirection,
    PaneCopySearchParams, PaneLinkPointParams, PaneSelectionReadParams, PaneTextPoint,
    PaneTextRange, SplitDirection,
};

fn client_for(server: &FakeHerdr) -> Client {
    Client::unix(server.sock_path.clone())
}

fn point(row: u32, col: u16) -> PaneTextPoint {
    PaneTextPoint { row, col }
}

// -- pane.copy_search ---------------------------------------------------------

#[tokio::test]
async fn pane_copy_search_serializes_full_shape() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "pane_copy_search",
        "pane_id": "wE:p1",
        "content_revision": 930,
        "matches": [
            {"start": {"row": 2, "col": 12}, "end": {"row": 2, "col": 16}},
            {"start": {"row": 12, "col": 6}, "end": {"row": 12, "col": 10}}
        ],
        "total": 2,
        "current": 0,
        "current_global": 0
    })))
    .await;
    let client = client_for(&server);

    let result = client
        .pane_copy_search(
            &PaneCopySearchParams {
                pane_id: "wE:p1".to_owned(),
                query: "panic".to_owned(),
                direction: PaneCopySearchDirection::Backward,
                cursor: point(0, 0),
                content_revision: 930,
                previous: Some(PaneTextRange {
                    start: point(2, 12),
                    end: point(2, 16),
                }),
            },
            None,
        )
        .await
        .unwrap();

    let req = &server.requests()[0];
    assert_eq!(req.method, "pane.copy_search");
    assert_eq!(
        req.params,
        json!({
            "pane_id": "wE:p1",
            "query": "panic",
            "direction": "backward",
            "cursor": {"row": 0, "col": 0},
            "content_revision": 930,
            "previous": {"start": {"row": 2, "col": 12}, "end": {"row": 2, "col": 16}}
        })
    );
    assert_eq!(result.pane_id, "wE:p1");
    assert_eq!(result.content_revision, 930);
    assert_eq!(result.total, 2);
    assert_eq!(result.current, Some(0));
    assert_eq!(result.current_global, Some(0));
    assert_eq!(result.matches.len(), 2);
    assert_eq!(result.matches[0].start, point(2, 12));
    assert_eq!(result.matches[0].end, point(2, 16));
    // A successful call is positive evidence for the feature.
    assert_eq!(
        client.feature(features::PANE_COPY_SEARCH).state,
        FeatureState::Supported
    );
}

#[tokio::test]
async fn pane_copy_search_omits_absent_previous() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "pane_copy_search",
        "pane_id": "wE:p1",
        "content_revision": 1,
        "matches": [],
        "total": 0,
        "current": null,
        "current_global": null
    })))
    .await;
    let client = client_for(&server);

    let result = client
        .pane_copy_search(
            &PaneCopySearchParams {
                pane_id: "wE:p1".to_owned(),
                query: "gone".to_owned(),
                direction: PaneCopySearchDirection::Forward,
                cursor: point(0, 0),
                content_revision: 1,
                previous: None,
            },
            None,
        )
        .await
        .unwrap();

    let p = &server.requests()[0].params;
    assert_eq!(p["direction"], "forward");
    assert!(
        p.get("previous").is_none(),
        "absent previous stays off the wire"
    );
    assert!(result.matches.is_empty());
    assert_eq!(result.current, None);
    assert_eq!(result.current_global, None);
}

#[tokio::test]
async fn pane_copy_search_stale_refusal_surfaces() {
    let server = FakeHerdr::start(Action::Refuse(
        "stale_content",
        "content changed since revision",
    ))
    .await;
    let client = client_for(&server);

    let err = client
        .pane_copy_search(
            &PaneCopySearchParams {
                pane_id: "wE:p1".to_owned(),
                query: "panic".to_owned(),
                direction: PaneCopySearchDirection::Forward,
                cursor: point(0, 0),
                content_revision: 12,
                previous: None,
            },
            None,
        )
        .await
        .unwrap_err();
    match err {
        HerdrError::Refused { code, .. } => assert_eq!(code, "stale_content"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    // A recognized non-unknown-method refusal is NOT negative evidence.
    assert_eq!(
        client.feature(features::PANE_COPY_SEARCH).state,
        FeatureState::Unknown
    );
}

#[tokio::test]
async fn pane_copy_search_unknown_method_marks_unsupported() {
    let server = FakeHerdr::start(Action::Refuse("unknown_method", "no such method")).await;
    let client = client_for(&server);

    let err = client
        .pane_copy_search(
            &PaneCopySearchParams {
                pane_id: "wE:p1".to_owned(),
                query: "x".to_owned(),
                direction: PaneCopySearchDirection::Forward,
                cursor: point(0, 0),
                content_revision: 1,
                previous: None,
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.phase(), DispatchPhase::Refused);
    assert_eq!(
        client.feature(features::PANE_COPY_SEARCH).state,
        FeatureState::Unsupported
    );
}

// -- pane.copy_motion (the revision probe) ------------------------------------

#[tokio::test]
async fn pane_copy_motion_probe_serializes_and_decodes() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "pane_copy_motion",
        "pane_id": "wE:p1",
        "cursor": {"row": 0, "col": 7},
        "content_revision": 930
    })))
    .await;
    let client = client_for(&server);

    let result = client
        .pane_copy_motion(
            &PaneCopyMotionParams {
                pane_id: "wE:p1".to_owned(),
                cursor: point(0, 0),
                motion: PaneCopyMotion::LineEnd,
                content_revision: None,
            },
            None,
        )
        .await
        .unwrap();

    let req = &server.requests()[0];
    assert_eq!(req.method, "pane.copy_motion");
    assert_eq!(
        req.params,
        json!({
            "pane_id": "wE:p1",
            "cursor": {"row": 0, "col": 0},
            "motion": "line_end"
        })
    );
    assert_eq!(result.cursor, point(0, 7));
    assert_eq!(result.content_revision, 930);
}

// -- pane.selection.read ------------------------------------------------------

#[tokio::test]
async fn pane_selection_read_serializes_and_decodes() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "pane_selection",
        "pane_id": "wE:p1",
        "text": "selected text"
    })))
    .await;
    let client = client_for(&server);

    let result = client
        .pane_selection_read(
            &PaneSelectionReadParams {
                pane_id: "wE:p1".to_owned(),
                anchor: point(1, 3),
                cursor: point(4, 9),
                content_revision: Some(41),
            },
            None,
        )
        .await
        .unwrap();

    let req = &server.requests()[0];
    assert_eq!(req.method, "pane.selection.read");
    assert_eq!(
        req.params,
        json!({
            "pane_id": "wE:p1",
            "anchor": {"row": 1, "col": 3},
            "cursor": {"row": 4, "col": 9},
            "content_revision": 41
        })
    );
    assert_eq!(result.text, "selected text");
}

#[tokio::test]
async fn pane_selection_read_unfenced_omits_revision() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "pane_selection",
        "pane_id": "wE:p1",
        "text": ""
    })))
    .await;
    let client = client_for(&server);

    client
        .pane_selection_read(
            &PaneSelectionReadParams {
                pane_id: "wE:p1".to_owned(),
                anchor: point(0, 0),
                cursor: point(0, 4),
                content_revision: None,
            },
            None,
        )
        .await
        .unwrap();

    let p = &server.requests()[0].params;
    assert!(
        p.get("content_revision").is_none(),
        "an unfenced read omits the field"
    );
}

// -- pane.link.* ---------------------------------------------------------------

fn link_params() -> PaneLinkPointParams {
    PaneLinkPointParams {
        pane_id: "wE:p1".to_owned(),
        viewport_row: 5,
        col: 12,
        content_revision: Some(41),
        offset_from_bottom: Some(3),
    }
}

#[tokio::test]
async fn pane_link_resolve_serializes_and_decodes_regions() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "pane_link_resolved",
        "regions": [{"row": 5, "start_col": 9, "end_col": 27}]
    })))
    .await;
    let client = client_for(&server);

    let result = client
        .pane_link_resolve(&link_params(), None)
        .await
        .unwrap();

    let req = &server.requests()[0];
    assert_eq!(req.method, "pane.link.resolve");
    assert_eq!(
        req.params,
        json!({
            "pane_id": "wE:p1",
            "viewport_row": 5,
            "col": 12,
            "content_revision": 41,
            "offset_from_bottom": 3
        })
    );
    assert_eq!(result.regions.len(), 1);
    assert_eq!(result.regions[0].row, 5);
    assert_eq!(result.regions[0].start_col, 9);
    assert_eq!(result.regions[0].end_col, 27);
}

#[tokio::test]
async fn pane_link_resolve_omits_optionals() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "pane_link_resolved",
        "regions": []
    })))
    .await;
    let client = client_for(&server);

    client
        .pane_link_resolve(
            &PaneLinkPointParams {
                pane_id: "wE:p1".to_owned(),
                viewport_row: 0,
                col: 0,
                content_revision: None,
                offset_from_bottom: None,
            },
            None,
        )
        .await
        .unwrap();

    let p = &server.requests()[0].params;
    assert!(p.get("content_revision").is_none());
    assert!(p.get("offset_from_bottom").is_none());
}

#[tokio::test]
async fn pane_link_activate_decodes_handled_and_url() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "pane_link_activated",
        "handled": true,
        "url": "https://example.com"
    })))
    .await;
    let client = client_for(&server);

    let result = client
        .pane_link_activate(&link_params(), None)
        .await
        .unwrap();
    assert_eq!(server.requests()[0].method, "pane.link.activate");
    assert!(result.handled);
    assert_eq!(result.url.as_deref(), Some("https://example.com"));
    assert_eq!(
        client.feature(features::PANE_LINK_ACTIVATE).state,
        FeatureState::Supported
    );
}

#[tokio::test]
async fn pane_link_activate_unhandled_carries_url() {
    // 0.9.1 answers the target string even when no handler took it.
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "pane_link_activated",
        "handled": false,
        "url": "https://unopened.example"
    })))
    .await;
    let client = client_for(&server);

    let result = client
        .pane_link_activate(&link_params(), None)
        .await
        .unwrap();
    assert!(!result.handled);
    assert_eq!(result.url.as_deref(), Some("https://unopened.example"));
}

#[tokio::test]
async fn pane_link_stale_target_refusal_surfaces() {
    let server =
        FakeHerdr::start(Action::Refuse("stale_target", "pane is no longer visible")).await;
    let client = client_for(&server);

    let err = client
        .pane_link_resolve(&link_params(), None)
        .await
        .unwrap_err();
    match err {
        HerdrError::Refused { code, .. } => assert_eq!(code, "stale_target"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

// -- layout.export / layout.apply ---------------------------------------------

fn layout_reply() -> serde_json::Value {
    json!({
        "type": "layout_export",
        "layout": {
            "workspace_id": "wE",
            "tab_id": "wE:t1",
            "zoomed": false,
            "focused_pane_id": "wE:p1",
            "root": {
                "type": "split",
                "direction": "right",
                "ratio": 0.5,
                "first": {"type": "pane", "pane_id": "wE:p1"},
                "second": {"type": "pane", "pane_id": "wE:p2"}
            }
        }
    })
}

#[tokio::test]
async fn layout_export_serializes_and_decodes() {
    let server = FakeHerdr::start(Action::Reply(layout_reply())).await;
    let client = client_for(&server);

    let layout = client
        .layout_export(
            &LayoutExportParams {
                pane_id: None,
                tab_id: Some("wE:t1".to_owned()),
            },
            None,
        )
        .await
        .unwrap();

    let req = &server.requests()[0];
    assert_eq!(req.method, "layout.export");
    assert_eq!(req.params, json!({ "tab_id": "wE:t1" }));
    assert_eq!(layout.workspace_id, "wE");
    assert_eq!(layout.tab_id, "wE:t1");
    assert_eq!(layout.focused_pane_id, "wE:p1");
    match &layout.root {
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            assert_eq!(*direction, SplitDirection::Right);
            assert_eq!(*ratio, 0.5);
            assert!(matches!(**first, LayoutNode::Pane { .. }));
            assert!(matches!(**second, LayoutNode::Pane { .. }));
        }
        other => panic!("expected a split root, got {other:?}"),
    }
}

#[tokio::test]
async fn layout_export_empty_params_stay_empty() {
    let server = FakeHerdr::start(Action::Reply(layout_reply())).await;
    let client = client_for(&server);

    client
        .layout_export(&LayoutExportParams::default(), None)
        .await
        .unwrap();
    assert_eq!(server.requests()[0].params, json!({}));
}

#[tokio::test]
async fn layout_apply_serializes_full_shape() {
    let server = FakeHerdr::start(Action::Reply(json!({
        "type": "layout_apply",
        "layout": layout_reply()["layout"].clone()
    })))
    .await;
    let client = client_for(&server);

    let layout = client
        .layout_apply(LayoutApplyParams {
            root: LayoutNode::Split {
                direction: SplitDirection::Down,
                ratio: 0.3,
                first: Box::new(LayoutNode::Pane {
                    pane_id: Some("wE:p1".to_owned()),
                    label: None,
                    cwd: None,
                    env: Default::default(),
                    command: None,
                }),
                second: Box::new(LayoutNode::Pane {
                    pane_id: None,
                    label: None,
                    cwd: Some("/tmp".to_owned()),
                    env: Default::default(),
                    command: Some(vec!["bash".to_owned()]),
                }),
            },
            workspace_id: Some("wE".to_owned()),
            tab_id: Some("wE:t2".to_owned()),
            tab_label: Some("rebuilt".to_owned()),
            focus: true,
        })
        .await
        .unwrap();

    let req = &server.requests()[0];
    assert_eq!(req.method, "layout.apply");
    assert_eq!(
        req.params,
        json!({
            "root": {
                "type": "split",
                "direction": "down",
                "ratio": 0.3,
                "first": {"type": "pane", "pane_id": "wE:p1"},
                "second": {"type": "pane", "cwd": "/tmp", "command": ["bash"]}
            },
            "workspace_id": "wE",
            "tab_id": "wE:t2",
            "tab_label": "rebuilt",
            "focus": true
        })
    );
    assert_eq!(layout.tab_id, "wE:t1");
    assert_eq!(
        client.feature(features::LAYOUT_APPLY).state,
        FeatureState::Supported
    );
}
