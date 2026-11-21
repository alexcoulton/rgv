use crate::Viewer;
use crate::automation::commands::evidence::bounded_automation_text;
use anyhow::{Context as _, Result};
use rgv_core::{
    AutomationHistoryNavigation, AutomationReviewNavigation, MAXIMUM_AUTOMATION_HISTORY_ENTRIES,
    MAXIMUM_REVIEW_ITEMS, PanelId, ReviewStatus,
};

impl Viewer {
    pub(crate) fn automation_review_queue_snapshot(&self) -> serde_json::Value {
        let active = self.panels.active();
        let mut items = self
            .review_items
            .iter()
            .take(MAXIMUM_REVIEW_ITEMS)
            .map(|item| {
                serde_json::json!({
                    "id": item.id,
                    "label": bounded_automation_text(&item.label, 4_096),
                    "locus": format!("{}:{}-{}", item.locus.contig, item.locus.start.saturating_add(1), item.locus.end),
                    "contig": item.locus.contig,
                    "start": item.locus.start,
                    "end": item.locus.end,
                    "status": item.status,
                    "current": active.contig_name == item.locus.contig
                        && active.viewport.interval().start() == item.locus.start
                        && active.viewport.interval().end() == item.locus.end,
                })
            })
            .collect::<Vec<_>>();
        let truncated = self.review_items.len() > items.len();
        items.shrink_to_fit();
        serde_json::json!({
            "application_instance_id": self.application_instance_id,
            "workspace_revision": self.workspace_revision,
            "items": items,
            "truncated": truncated,
        })
    }

    pub(crate) fn automation_navigation_history_snapshot(
        &self,
        panel_id: PanelId,
    ) -> Result<serde_json::Value> {
        let panel = self
            .panels
            .get(panel_id)
            .with_context(|| format!("panel {} was not found", panel_id.get()))?;
        let entries = panel
            .navigation_history
            .entries()
            .enumerate()
            .take(MAXIMUM_AUTOMATION_HISTORY_ENTRIES)
            .map(|(index, entry)| {
                serde_json::json!({
                    "index": index,
                    "locus": format!(
                        "{}:{}-{}",
                        entry.contig_name,
                        entry.start.saturating_add(1),
                        entry.end
                    ),
                    "contig": entry.contig_name,
                    "start": entry.start,
                    "end": entry.end,
                    "current": index == panel.navigation_history.cursor(),
                })
            })
            .collect::<Vec<_>>();
        Ok(serde_json::json!({
            "application_instance_id": self.application_instance_id,
            "workspace_revision": self.workspace_revision,
            "panel_id": panel_id.get(),
            "entries": entries,
            "cursor": panel.navigation_history.cursor(),
            "can_go_back": panel.navigation_history.can_go_back(),
            "can_go_forward": panel.navigation_history.can_go_forward(),
            "truncated": panel.navigation_history.len() > MAXIMUM_AUTOMATION_HISTORY_ENTRIES,
        }))
    }

    pub(crate) fn apply_automation_review_navigation(
        &mut self,
        action: &AutomationReviewNavigation,
    ) -> Result<serde_json::Value> {
        let item = match action {
            AutomationReviewNavigation::Item { review_item_id } => self
                .review_items
                .iter()
                .find(|item| item.id == *review_item_id),
            AutomationReviewNavigation::NextPending => self
                .review_items
                .iter()
                .find(|item| item.status == ReviewStatus::Pending),
        }
        .cloned()
        .context("review item was not found")?;
        let locus = format!(
            "{}:{}-{}",
            item.locus.contig,
            item.locus.start.saturating_add(1),
            item.locus.end
        );
        self.navigate_to_locus(&locus)?;
        self.bump_workspace_revision();
        Ok(serde_json::json!({
            "review_item_id": item.id,
            "locus": locus,
            "start": item.locus.start,
            "end": item.locus.end,
            "settled": self.automation_is_settled(),
            "workspace_revision": self.workspace_revision,
        }))
    }

    pub(crate) fn apply_automation_history_navigation(
        &mut self,
        panel_id: PanelId,
        action: AutomationHistoryNavigation,
    ) -> Result<serde_json::Value> {
        anyhow::ensure!(
            self.panels.activate(panel_id),
            "panel {} was not found",
            panel_id.get()
        );
        self.commit_navigation_gesture();
        let entry = match action {
            AutomationHistoryNavigation::Back => self.panels.active_mut().navigation_history.back(),
            AutomationHistoryNavigation::Forward => {
                self.panels.active_mut().navigation_history.forward()
            }
            AutomationHistoryNavigation::Index { index } => {
                self.panels.active_mut().navigation_history.select(index)
            }
        }
        .cloned()
        .context("navigation history boundary was reached")?;
        let interval = self.navigate_to_history_entry(&entry)?;
        self.bump_workspace_revision();
        Ok(serde_json::json!({
            "panel_id": panel_id.get(),
            "contig": self.panels.active().contig_name,
            "start": interval.start(),
            "end": interval.end(),
            "settled": self.automation_is_settled(),
            "workspace_revision": self.workspace_revision,
        }))
    }
}
