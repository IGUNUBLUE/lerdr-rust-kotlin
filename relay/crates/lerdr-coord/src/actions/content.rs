//! Pane-content families — the Phase-5 §1.2-1.5 actions (docs/13):
//! `pane_search` and `pane_selection_read` (the fenced copy family,
//! capability `pane_search`), `pane_link_resolve`/`pane_link_activate`
//! (capability `pane_links`), and `layout_export`/`layout_apply`
//! (capability `layout`).
//!
//! Read fence (docs/13 §3): the fenced family reuses `pane_read_fresh`'s
//! upstream `content_revision` watermark — [`Topology::upstream_rev_of`]
//! is the pane's one upstream content clock, fed by `PaneInfo.revision`,
//! `pane_output_changed` events, and `pane.read` results; the copy
//! family's results report the same clock and fold back into it.
//! `pane.copy_search` *requires* a revision upstream: an unobserved
//! watermark (`0`) runs the unfenced `pane.copy_motion` probe first, and
//! a `stale_content` refusal re-probes and retries once — the served
//! watch watermark is authoritative relay-side, so the app never sends
//! a revision.
//!
//! Projection notes against upstream 0.9.1: `pane.link.resolve` answers
//! the link's cell *regions*, not a URL (the target string surfaces on
//! `pane.link.activate`'s `{handled,url}` — the spec's `{url}` resolve
//! reply is unimplementable and deliberately not faked); `layout.export`
//! answers the full `LayoutDescription` and the relay projects its
//! `root` per the spec shape.

use lerdr_core::protocol::{Inbound, Outbound};
use lerdr_herdr::{
    HerdrError, LayoutApplyParams, LayoutExportParams, LayoutNode, PaneCopyMotion,
    PaneCopyMotionParams, PaneCopySearchDirection, PaneCopySearchParams, PaneLinkPointParams,
    PaneSelectionReadParams, PaneTextPoint, PaneTextRange,
};
use serde_json::Value;

use super::{
    capability_gap, dispatch_failure, method_refuted, pane_of, ActionContext, Outcome,
    COMMAND_DEADLINE,
};

/// `pane_search` — server-side find over full scrollback. `cursor` and
/// `previous` are the copy-engine's `{row,col}`/`{start,end}` objects,
/// read off the raw map (`cursor` is a *string* field for the legacy
/// pagination actions — `Inbound` tolerates the object shape).
pub(crate) async fn pane_search(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let pane_id = pane_of(message).to_owned();
    let parsed = (|| -> Result<(String, PaneCopySearchDirection, PaneTextPoint, Option<PaneTextRange>), String> {
        let query = message
            .raw_str("query")
            .filter(|query| !query.is_empty())
            .ok_or_else(|| "Query is required".to_owned())?
            .to_owned();
        let direction = match message.direction.as_str() {
            "" | "forward" => PaneCopySearchDirection::Forward,
            "backward" => PaneCopySearchDirection::Backward,
            _ => return Err("Direction must be forward or backward".to_owned()),
        };
        let cursor = point(message, "cursor")?;
        let previous = previous_range(message)?;
        Ok((query, direction, cursor, previous))
    })();
    let outcome = if pane_id.is_empty() {
        Outcome::failed(&pane_id, "Pane is required")
    } else {
        match parsed {
            Err(error) => Outcome::failed(&pane_id, error),
            Ok(_) if method_refuted(&ctx, "pane.copy_search") => {
                capability_gap("pane.copy_search", &pane_id)
            }
            Ok((query, direction, cursor, previous)) => {
                // The fence is required upstream — learn it through the
                // unfenced probe when the watermark is unobserved.
                let rev = match content_revision(&ctx, &pane_id).await {
                    Ok(rev) => rev,
                    Err(err) => {
                        let outcome = dispatch_failure(&pane_id, &err);
                        return outcome.frames(request_id, "pane_search", action_id);
                    }
                };
                let client = ctx.client.clone();
                let target = pane_id.clone();
                let result = fenced(&ctx, &pane_id, rev, |rev| {
                    let (client, target, query) = (client.clone(), target.clone(), query.clone());
                    async move {
                        client
                            .pane_copy_search(
                                &PaneCopySearchParams {
                                    pane_id: target,
                                    query,
                                    direction,
                                    cursor,
                                    content_revision: rev,
                                    previous,
                                },
                                Some(COMMAND_DEADLINE),
                            )
                            .await
                    }
                })
                .await;
                match result {
                    Ok(result) => {
                        // The result reports the same upstream clock —
                        // fold it into the served watermark.
                        ctx.topology
                            .note_upstream_rev(&pane_id, result.content_revision);
                        Outcome::completed(
                            &pane_id,
                            Some(serde_json::json!({
                                "matches": result.matches,
                                "content_revision": result.content_revision,
                                "total": result.total,
                                "current": result.current,
                                "current_global": result.current_global,
                            })),
                        )
                    }
                    Err(err) => dispatch_failure(&pane_id, &err),
                }
            }
        }
    };
    outcome.frames(request_id, "pane_search", action_id)
}

/// `pane_selection_read` — an arbitrary copy-engine range. The fence is
/// optional upstream: the watermark rides along when known (`None`
/// reads unfenced).
pub(crate) async fn pane_selection_read(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let pane_id = pane_of(message).to_owned();
    let parsed = point(message, "anchor")
        .and_then(|anchor| point(message, "cursor").map(|cursor| (anchor, cursor)));
    let outcome = if pane_id.is_empty() {
        Outcome::failed(&pane_id, "Pane is required")
    } else {
        match parsed {
            Err(error) => Outcome::failed(&pane_id, error),
            Ok(_) if method_refuted(&ctx, "pane.selection.read") => {
                capability_gap("pane.selection.read", &pane_id)
            }
            Ok((anchor, cursor)) => {
                let rev = ctx.topology.upstream_rev_of(&pane_id);
                let params = |fence: Option<u64>| PaneSelectionReadParams {
                    pane_id: pane_id.clone(),
                    anchor,
                    cursor,
                    content_revision: fence,
                };
                let result = fenced_opt(&ctx, &pane_id, rev, |fence| {
                    let client = ctx.client.clone();
                    let params = params(fence);
                    async move {
                        client
                            .pane_selection_read(&params, Some(COMMAND_DEADLINE))
                            .await
                    }
                })
                .await;
                match result {
                    Ok((result, fence)) => Outcome::completed(
                        &pane_id,
                        Some(serde_json::json!({
                            "text": result.text,
                            "content_revision": fence.unwrap_or(0),
                        })),
                    ),
                    Err(err) => dispatch_failure(&pane_id, &err),
                }
            }
        }
    };
    outcome.frames(request_id, "pane_selection_read", action_id)
}

/// `pane_link_resolve` — hit-test a viewport cell for a link. `row`/`col`
/// are the last served frame's viewport coordinates; `offset_from_bottom`
/// rides the pane's upstream scroll offset so a scrolled-back cell
/// resolves in scrollback space.
pub(crate) async fn pane_link_resolve(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let outcome = match link_point(&ctx, message, "pane.link.resolve") {
        Err(outcome) => *outcome,
        Ok((pane_id, params)) => {
            let rev = ctx.topology.upstream_rev_of(&pane_id);
            let result = fenced_opt(&ctx, &pane_id, rev, |fence| {
                let client = ctx.client.clone();
                let params = PaneLinkPointParams {
                    content_revision: fence,
                    ..params.clone()
                };
                async move {
                    client
                        .pane_link_resolve(&params, Some(COMMAND_DEADLINE))
                        .await
                }
            })
            .await;
            match result {
                Ok((result, _)) => Outcome::completed(
                    &pane_id,
                    Some(serde_json::json!({ "regions": result.regions })),
                ),
                Err(err) => dispatch_failure(&pane_id, &err),
            }
        }
    };
    outcome.frames(request_id, "pane_link_resolve", action_id)
}

/// `pane_link_activate` — open the link at the viewport cell in the
/// desktop browser. Same addressing as resolve; the reply carries
/// `{handled,url}` — `url` is present even when no handler took it.
pub(crate) async fn pane_link_activate(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let outcome = match link_point(&ctx, message, "pane.link.activate") {
        Err(outcome) => *outcome,
        Ok((pane_id, params)) => {
            let rev = ctx.topology.upstream_rev_of(&pane_id);
            let result = fenced_opt(&ctx, &pane_id, rev, |fence| {
                let client = ctx.client.clone();
                let params = PaneLinkPointParams {
                    content_revision: fence,
                    ..params.clone()
                };
                async move {
                    client
                        .pane_link_activate(&params, Some(COMMAND_DEADLINE))
                        .await
                }
            })
            .await;
            match result {
                Ok((result, _)) => {
                    // Activation may move desktop focus — republish so the
                    // change reaches the phone without the next event.
                    ctx.handle.refresh().await;
                    Outcome::completed(
                        &pane_id,
                        Some(serde_json::json!({
                            "handled": result.handled,
                            "url": result.url,
                        })),
                    )
                }
                Err(err) => dispatch_failure(&pane_id, &err),
            }
        }
    };
    outcome.frames(request_id, "pane_link_activate", action_id)
}

/// `layout_export` — the pane/tab's layout tree. `target.tab_id` and
/// `target.pane_id` are the addresses (either may be absent — upstream
/// exports the focused tab when neither is set). The reply projects the
/// description's `root` per the spec shape.
pub(crate) async fn layout_export(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let pane_id = pane_of(message).to_owned();
    let tab_id = message
        .target
        .as_ref()
        .map(|target| target.tab_id.clone())
        .unwrap_or_default();
    let outcome = if method_refuted(&ctx, "layout.export") {
        capability_gap("layout.export", &pane_id)
    } else {
        match ctx
            .client
            .layout_export(
                &LayoutExportParams {
                    pane_id: (!pane_id.is_empty()).then(|| pane_id.clone()),
                    tab_id: (!tab_id.is_empty()).then_some(tab_id),
                },
                Some(COMMAND_DEADLINE),
            )
            .await
        {
            Ok(layout) => {
                Outcome::completed(&pane_id, Some(serde_json::json!({ "root": layout.root })))
            }
            Err(err) => dispatch_failure(&pane_id, &err),
        }
    };
    outcome.frames(request_id, "layout_export", action_id)
}

/// `layout_apply` — rebuild a layout from the exported tree. `root` is
/// herdr's `LayoutNode` verbatim; `workspace_id`/`tab_id`/`tab_label`/
/// `focus` are top-level fields (nullable on the wire). Audited — the
/// spawned task writes the result row through `sendAuditedCommandResult`.
pub(crate) async fn layout_apply(
    ctx: ActionContext,
    request_id: &str,
    action_id: &str,
    message: &Inbound,
) -> Vec<Outbound> {
    let pane_id = pane_of(message).to_owned();
    let root = match message.raw("root") {
        None | Some(Value::Null) => Err("Layout root is required"),
        Some(value) => serde_json::from_value::<LayoutNode>(value.clone())
            .map_err(|_| "Layout root is invalid"),
    };
    let outcome = match root {
        Err(error) => Outcome::failed(&pane_id, error),
        Ok(_) if method_refuted(&ctx, "layout.apply") => capability_gap("layout.apply", &pane_id),
        Ok(root) => {
            let params = LayoutApplyParams {
                root,
                workspace_id: optional_str(
                    message.workspace_id.as_str(),
                    message.target.as_ref().map(|t| t.workspace_id.as_str()),
                ),
                tab_id: optional_str(
                    message.raw_str("tab_id").unwrap_or_default(),
                    message.target.as_ref().map(|t| t.tab_id.as_str()),
                ),
                tab_label: message
                    .raw_str("tab_label")
                    .filter(|label| !label.is_empty())
                    .map(str::to_owned),
                focus: message
                    .raw("focus")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            match ctx.client.layout_apply(params).await {
                Ok(layout) => {
                    // The topology changed wholesale — republish.
                    ctx.handle.refresh().await;
                    Outcome::completed(&pane_id, Some(serde_json::json!({ "layout": layout })))
                }
                Err(err) => dispatch_failure(&pane_id, &err),
            }
        }
    };
    outcome.frames(request_id, "layout_apply", action_id)
}

/// `pane_search`/`pane_selection_read` fence: the pane's upstream
/// `content_revision`, learned via the unfenced `pane.copy_motion` probe
/// when the watermark is unobserved (`pane.copy_search` *requires* a
/// revision — `0` cannot be sent as a guess because a pane at any real
/// epoch would refuse it stale).
async fn content_revision(ctx: &ActionContext, pane_id: &str) -> Result<u64, HerdrError> {
    let known = ctx.topology.upstream_rev_of(pane_id);
    if known != 0 {
        return Ok(known);
    }
    probe_content_revision(ctx, pane_id).await
}

/// `pane.copy_motion` with no fence — the copy family's revision probe.
/// The observed revision folds into the shared watermark so the next
/// fenced call (or a concurrent one) starts from the fresh mark.
async fn probe_content_revision(ctx: &ActionContext, pane_id: &str) -> Result<u64, HerdrError> {
    let result = ctx
        .client
        .pane_copy_motion(
            &PaneCopyMotionParams {
                pane_id: pane_id.to_owned(),
                cursor: PaneTextPoint { row: 0, col: 0 },
                motion: PaneCopyMotion::LineEnd,
                content_revision: None,
            },
            Some(COMMAND_DEADLINE),
        )
        .await?;
    ctx.topology
        .note_upstream_rev(pane_id, result.content_revision);
    Ok(result.content_revision)
}

/// Run `attempt(rev)` at `initial_rev`; a `stale_content` refusal
/// re-probes the watermark and retries once — the fence is the only
/// moving part between attempts.
async fn fenced<T, Fut>(
    ctx: &ActionContext,
    pane_id: &str,
    initial_rev: u64,
    attempt: impl Fn(u64) -> Fut,
) -> Result<T, HerdrError>
where
    Fut: std::future::Future<Output = Result<T, HerdrError>>,
{
    match attempt(initial_rev).await {
        Err(err) if is_stale(&err) => {
            let fresh = probe_content_revision(ctx, pane_id).await?;
            attempt(fresh).await
        }
        result => result,
    }
}

/// The optional-fence variant: `0` means "unfenced" upstream
/// (`content_revision` omitted); a stale refusal still re-probes and
/// retries once, now fenced. Returns the winning result plus the fence
/// it ran under (`None` = unfenced).
async fn fenced_opt<T, Fut>(
    ctx: &ActionContext,
    pane_id: &str,
    initial_rev: u64,
    attempt: impl Fn(Option<u64>) -> Fut,
) -> Result<(T, Option<u64>), HerdrError>
where
    Fut: std::future::Future<Output = Result<T, HerdrError>>,
{
    let fence = (initial_rev != 0).then_some(initial_rev);
    match attempt(fence).await {
        Err(err) if is_stale(&err) => {
            let fresh = probe_content_revision(ctx, pane_id).await?;
            attempt(Some(fresh))
                .await
                .map(|result| (result, Some(fresh)))
        }
        result => result.map(|result| (result, fence)),
    }
}

/// The copy family's stale-fence refusal.
fn is_stale(err: &HerdrError) -> bool {
    err.refusal_code() == Some("stale_content")
}

/// A required `{row,col}` object off the raw map — the copy-engine point
/// the Phase-5 wire uses for `cursor`/`anchor`/`previous.{start,end}`.
fn point(message: &Inbound, key: &str) -> Result<PaneTextPoint, String> {
    match message.raw(key) {
        None | Some(Value::Null) => Err(format!("{key} is required")),
        Some(value) => serde_json::from_value::<PaneTextPoint>(value.clone())
            .map_err(|_| format!("{key} must be a {{row,col}} object")),
    }
}

/// `previous` — absent/`null` unanchors the search; a present object must
/// decode as the `{start,end}` range.
fn previous_range(message: &Inbound) -> Result<Option<PaneTextRange>, String> {
    match message.raw("previous") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value::<PaneTextRange>(value.clone())
            .map(Some)
            .map_err(|_| "previous must be a {start,end} object".to_owned()),
    }
}

/// The link actions' shared preamble: pane id, `row`/`col` viewport
/// coordinates (u16-bounded), the per-method refutation check, and the
/// params skeleton — `offset_from_bottom` rides the pane's upstream
/// scroll offset (the served frame's row base); the content-revision
/// fence is layered on at dispatch. `Err` is boxed — `Outcome` is the
/// big result-frames struct, not a compact error.
fn link_point(
    ctx: &ActionContext,
    message: &Inbound,
    method: &'static str,
) -> Result<(String, PaneLinkPointParams), Box<Outcome>> {
    let pane_id = pane_of(message).to_owned();
    if pane_id.is_empty() {
        return Err(Box::new(Outcome::failed(&pane_id, "Pane is required")));
    }
    let (row, col) = match (message.raw_int("row"), message.raw_int("col")) {
        (Some(row), Some(col))
            if (0..=i64::from(u16::MAX)).contains(&row)
                && (0..=i64::from(u16::MAX)).contains(&col) =>
        {
            (row as u16, col as u16)
        }
        _ => {
            return Err(Box::new(Outcome::failed(
                &pane_id,
                "Row and col are required",
            )))
        }
    };
    if method_refuted(ctx, method) {
        return Err(Box::new(capability_gap(method, &pane_id)));
    }
    Ok((
        pane_id.clone(),
        PaneLinkPointParams {
            offset_from_bottom: ctx.topology.pane_scroll_offset(&pane_id),
            pane_id,
            viewport_row: row,
            col,
            content_revision: None,
        },
    ))
}

/// First non-empty of a top-level and a `target` string — `layout_apply`'s
/// addressing fields arrive top-level but `target.*` is honored too.
fn optional_str(primary: &str, fallback: Option<&str>) -> Option<String> {
    if !primary.is_empty() {
        return Some(primary.to_owned());
    }
    fallback
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
