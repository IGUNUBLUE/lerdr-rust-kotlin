//! History recovery through the relay's own native controller. Herdr refuses
//! its external text harvester while a controller owns the terminal, so keep
//! that authority and grid, use bounded wheel steps, and restore before ungating.

use std::time::Duration;

use lerdr_herdr::{PaneReadResult, ReadFormat, ReadSource};
use tokio::time::{sleep, timeout_at, Instant};

use crate::application_history::Epoch;
use crate::pane_stream::{application_row, HostHistory};
use crate::watches::{application_epoch, WatchDeps, MAX_PANE_LINES};

const PHASE_LIMIT: Duration = Duration::from_secs(8);
const MAX_STEPS: usize = 64;

fn rows(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

fn plain(rows: &[String]) -> Vec<String> {
    rows.iter().map(|row| application_row(row)).collect()
}

fn restored_view(initial: &[String], current: &[String]) -> bool {
    // TUI status/input footers can update after the agent becomes idle.
    // Confirm the same body position without requiring that footer to freeze.
    let body = initial.len().saturating_sub(8).max(3);
    initial.len() == current.len()
        && initial
            .get(..body)
            .is_some_and(|rows| current.starts_with(rows))
}

/// New snapshots overlap the previous viewport, but can retain sticky headers
/// and footers. Insert only newly exposed rows at the shared header boundary.
fn merge_up(history: &mut Vec<String>, previous: &[String], next: &[String]) -> Option<usize> {
    let old = plain(previous);
    let new = plain(next);
    if old == new {
        return Some(0);
    }
    let header = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let mut nonblank = 0;
    let span = old[header..].iter().position(|row| {
        nonblank += usize::from(!row.is_empty());
        nonblank == 3
    })? + 1;
    let anchor = &old[header..header + span];
    let mut matches = new[header..]
        .windows(span)
        .enumerate()
        .filter_map(|(index, block)| (block == anchor).then_some(index));
    let Some(added) = matches.next() else {
        // At the beginning of a turn, a collapsed sticky prompt can expand.
        // Match an early body run instead, replacing that old header rather
        // than discarding the first answer row or duplicating the prompt.
        for start in 1..old.len() / 2 {
            let anchor = &old[start..start + 3];
            if anchor.iter().any(String::is_empty) {
                continue;
            }
            let mut positions = new
                .windows(3)
                .enumerate()
                .filter_map(|(i, block)| (block == anchor).then_some(i));
            let Some(at) = positions.next() else { continue };
            if positions.next().is_none() && at > start && plain(history).starts_with(&old[..start])
            {
                history.splice(..start, next[..at].iter().cloned());
                return Some(at - start);
            }
        }
        return None;
    };
    if matches.next().is_some() || added == 0 {
        return None;
    }
    let insertion = if plain(history).starts_with(&old[..header]) {
        header
    } else {
        0
    };
    history.splice(
        insertion..insertion,
        next[header..header + added].iter().cloned(),
    );
    Some(added)
}

async fn settled_visible(
    pane: &str,
    deps: &WatchDeps,
    deadline: Instant,
    before: Option<&[String]>,
) -> Option<PaneReadResult> {
    timeout_at(deadline, async {
        // Wait for the wheel's redraw, then require a quiet pair of snapshots.
        sleep(Duration::from_millis(100)).await;
        let mut previous = deps
            .handle
            .client
            .pane_read(pane, ReadSource::Visible, MAX_PANE_LINES, ReadFormat::Ansi)
            .await
            .ok()?;
        let no_change_until = Instant::now() + Duration::from_millis(500);
        for _ in 0..30 {
            sleep(Duration::from_millis(20)).await;
            let next = deps
                .handle
                .client
                .pane_read(pane, ReadSource::Visible, MAX_PANE_LINES, ReadFormat::Ansi)
                .await
                .ok()?;
            if plain(&rows(&previous.text)) == plain(&rows(&next.text)) {
                // Dispatch is asynchronous. An unchanged quiet pair before
                // the TUI handles the wheel is not proof of top/restoration.
                if before.is_none_or(|before| plain(before) != plain(&rows(&next.text)))
                    || Instant::now() >= no_change_until
                {
                    return Some(next);
                }
            }
            previous = next;
        }
        None
    })
    .await
    .ok()
    .flatten()
}

pub(crate) async fn recover(
    pane: &str,
    deps: &WatchDeps,
    epoch: &Epoch,
    lines: u32,
) -> Option<HostHistory> {
    let events = 3;
    let deadline = Instant::now() + PHASE_LIMIT;
    let initial = settled_visible(pane, deps, deadline, None).await?;
    let initial_rows = rows(&initial.text);
    // Harvest only at the live bottom; do not treat a user-scrolled viewport
    // as the end of the transcript. Undo the probe before declining recovery.
    deps.leases.history_scroll(pane, false, 1).await.ok()?;
    let probed = settled_visible(pane, deps, deadline, Some(&initial_rows)).await;
    if !probed
        .as_ref()
        .is_some_and(|read| restored_view(&plain(&initial_rows), &plain(&rows(&read.text))))
    {
        deps.leases.history_scroll(pane, true, 1).await.ok()?;
        let before = probed.as_ref().map(|read| rows(&read.text));
        let restored = settled_visible(pane, deps, Instant::now() + PHASE_LIMIT, before.as_deref())
            .await
            .is_some_and(|read| restored_view(&plain(&initial_rows), &plain(&rows(&read.text))));
        if !restored {
            tracing::warn!(pane, "history bottom probe restoration not confirmed");
        }
        return restored.then(HostHistory::default);
    }
    let mut history = initial_rows.clone();
    let mut previous = initial_rows.clone();
    let mut steps = 0;
    let mut valid = true;
    let mut reached_top = false;
    while history.len() < lines as usize && steps < MAX_STEPS && Instant::now() < deadline {
        if deps.cancel.is_cancelled()
            || deps.leases.capture_size(pane).await != epoch.size
            || !application_epoch(pane, deps, epoch.size)
                .is_some_and(|(current, idle)| current == *epoch && idle)
        {
            valid = false;
            break;
        }
        // Count a possibly dispatched wheel so failure still runs restoration.
        steps += 1;
        if !matches!(
            timeout_at(deadline, deps.leases.history_scroll(pane, true, events)).await,
            Ok(Ok(()))
        ) {
            break;
        }
        let Some(next) = settled_visible(pane, deps, deadline, Some(&previous)).await else {
            break;
        };
        let next = rows(&next.text);
        match merge_up(&mut history, &previous, &next) {
            Some(0) => {
                reached_top = true;
                break;
            }
            Some(_) => previous = next,
            None => {
                // A prompt boundary can replace the sticky header and remove
                // the overlap. Keep only rows already proven by earlier
                // snapshots; never guess across that boundary.
                tracing::debug!(pane, rows = history.len(), "history overlap ended");
                break;
            }
        }
    }
    // Restoration is deliberately independent of the disconnected watch's
    // cancellation token. Its private geometry hold remains alive until here.
    let restore_deadline = Instant::now() + PHASE_LIMIT;
    let restore_events = epoch
        .size
        .map_or(12, |(_, rows)| (rows / 2).clamp(1, 32) as u8);
    let mut restored = false;
    let mut previous_restore = None;
    for _ in 0..=steps + 2 {
        let Some(current) =
            settled_visible(pane, deps, restore_deadline, previous_restore.as_deref()).await
        else {
            break;
        };
        let current = plain(&rows(&current.text));
        let changed_size = deps.leases.capture_size(pane).await != epoch.size;
        if restored_view(&plain(&initial_rows), &current)
            || (changed_size && previous_restore.as_ref() == Some(&current))
        {
            restored = true;
            break;
        }
        previous_restore = Some(current);
        if !matches!(
            timeout_at(
                restore_deadline,
                deps.leases.history_scroll(pane, false, restore_events)
            )
            .await,
            Ok(Ok(()))
        ) {
            break;
        }
    }
    if !restored {
        tracing::warn!(
            pane,
            "application history viewport restoration not confirmed"
        );
    }
    tracing::debug!(
        pane,
        steps,
        rows = history.len(),
        valid,
        restored,
        "controlled history recovery"
    );
    (valid && restored).then(|| HostHistory {
        // History uses plain rows: transient scrollbar chrome must not become
        // part of the transcript. The live viewport retains its ANSI styling.
        text: plain(&history)
            .into_iter()
            .map(|row| format!("{row}\n"))
            .collect(),
        truncated: !reached_top || initial.truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport(start: usize) -> Vec<String> {
        let mut rows = vec!["sticky prompt".to_owned()];
        rows.extend((start..start + 30).map(|n| format!("answer {n:03}")));
        rows.extend(["".to_owned(), "fixed input".to_owned()]);
        rows
    }

    #[test]
    fn scrollbar_appearing_on_first_wheel_does_not_break_the_overlap() {
        let old = viewport(71);
        let next: Vec<_> = viewport(68)
            .into_iter()
            .map(|row| format!("{row}          │"))
            .collect();
        let mut history = old.clone();
        assert_eq!(merge_up(&mut history, &old, &next), Some(3));
        let text = plain(&history).join("\n");
        for n in 68..=100 {
            assert_eq!(text.matches(&format!("answer {n:03}")).count(), 1);
        }
        assert!(restored_view(&plain(&old), &plain(&old)));
    }

    #[test]
    fn restoration_tolerates_footer_updates_but_requires_the_same_body() {
        let initial = viewport(71);
        let mut current = initial.clone();
        *current.last_mut().unwrap() = "input updated".into();
        assert!(restored_view(&initial, &current));
        current[1] = "answer 070".into();
        assert!(!restored_view(&initial, &current));
        assert!(!restored_view(&initial, &current[..20]));
    }

    #[test]
    fn expanded_prompt_at_turn_start_keeps_the_first_answer_row() {
        let old = vec![
            "collapsed prompt".into(),
            "answer 002".into(),
            "answer 003".into(),
            "answer 004".into(),
            "answer 005".into(),
            "input".into(),
        ];
        let new = vec![
            "expanded prompt".into(),
            "second prompt row".into(),
            "answer 001".into(),
            "answer 002".into(),
            "answer 003".into(),
            "answer 004".into(),
        ];
        let mut history = old.clone();
        assert_eq!(merge_up(&mut history, &old, &new), Some(2));
        assert_eq!(&history[..4], &new[..4]);
        assert_eq!(history.iter().filter(|r| *r == "answer 002").count(), 1);
    }

    #[test]
    fn controlled_scroll_merges_numbered_rows_once_and_preserves_fixed_ui() {
        let mut previous = viewport(71);
        let mut history = previous.clone();
        for start in [61, 51, 41, 31, 21, 11, 1] {
            let next = viewport(start);
            assert_eq!(merge_up(&mut history, &previous, &next), Some(10));
            previous = next;
        }
        assert_eq!(merge_up(&mut history, &previous, &previous), Some(0));
        let text = history.join("\n");
        for n in 1..=100 {
            assert_eq!(text.matches(&format!("answer {n:03}")).count(), 1)
        }
        assert_eq!(text.matches("sticky prompt").count(), 1);
        assert_eq!(text.matches("fixed input").count(), 1);
        assert!(merge_up(&mut history, &previous, &viewport(200)).is_none());
    }
}
