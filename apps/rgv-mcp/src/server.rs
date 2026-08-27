use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use rgv_core::{
    AUTOMATION_SCHEMA_VERSION, AlignmentColorMode, AlignmentGroupMode, AlignmentOrderDirection,
    AlignmentSortMode, AutomationAlignmentRelationship, AutomationAlignmentReveal,
    AutomationAlignmentSelector, AutomationAlignmentState, AutomationBatch, AutomationCommand,
    AutomationCommandStatus, AutomationExportKind, AutomationHistoryNavigation,
    AutomationInteractionTrace, AutomationNavigationDestination, AutomationReviewNavigation,
    AutomationRunReport, AutomationSelection, AutomationSelectionGranularity, AutomationSetting,
    AutomationSnapshotFormat, AutomationSnapshotScope, AutomationUnsavedChangesPolicy,
    AutomationViewportOperation, AutomationWorkspaceSnapshot, CoverageScaleTransform,
    ModifiedBaseColorMode, OverlappingMatePolicy, PanelId, PresentationNodeId, ReadBaseDisplayMode,
    TrackChannel, TrackId, TrackKind, TrackSource,
};
use rmcp::schemars;
use rmcp::{
    ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    schemars::JsonSchema,
    tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    artifacts::{ArtifactError, ArtifactStore},
    config::RgvMcpArgs,
    control_client::{ControlClient, ControlClientError},
};

const DEFAULT_SETTLE_TIMEOUT_MILLIS: u64 = 30_000;
const MAXIMUM_MCP_BATCH_COMMANDS: usize = 100;
const MAXIMUM_INLINE_EXPORT_BYTES: usize = 256 * 1024;

#[derive(Clone)]
pub struct RgvMcpServer {
    control: ControlClient,
    artifacts: ArtifactStore,
}

impl RgvMcpServer {
    pub fn from_args(args: RgvMcpArgs) -> anyhow::Result<Self> {
        let config = args.load()?;
        let control = ControlClient::new(config.control_url, config.control_secret)?;
        let artifacts = ArtifactStore::new(&config.control_root, &config.artifact_directory)?;
        Ok(Self { control, artifacts })
    }

    /// Performs a non-sensitive startup probe without making viewer availability
    /// a prerequisite for starting the stdio service.
    pub async fn viewer_is_compatible(&self) -> bool {
        self.control.status().await.is_ok_and(|status| {
            status.schema_version == AUTOMATION_SCHEMA_VERSION
                && status
                    .control_schema_version
                    .is_none_or(|version| version == AUTOMATION_SCHEMA_VERSION)
        })
    }

    async fn execute(&self, commands: Vec<AutomationCommand>) -> CallToolResult {
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands,
        };
        match self.control.run_batch(&batch).await {
            Ok(report) if report.succeeded => success_result(
                "RGV completed the requested live-window operation",
                json!({"report": public_report(&report)}),
            ),
            Ok(report) => report_failure(&report),
            Err(error) => control_failure(&error),
        }
    }

    async fn execute_with_data<T: serde::de::DeserializeOwned>(
        &self,
        commands: Vec<AutomationCommand>,
        result_index: usize,
        label: &str,
    ) -> Result<(T, AutomationRunReport), CallToolResult> {
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands,
        };
        let report = self
            .control
            .run_batch(&batch)
            .await
            .map_err(|error| control_failure(&error))?;
        if !report.succeeded {
            return Err(report_failure(&report));
        }
        let value = report
            .results
            .get(result_index)
            .map(|result| result.data.clone())
            .ok_or_else(|| {
                adapter_failure("invalid_response", "RGV omitted an expected command result")
            })?;
        let parsed = serde_json::from_value(value).map_err(|_| {
            adapter_failure(
                "invalid_response",
                &format!("RGV returned malformed {label} data"),
            )
        })?;
        Ok((parsed, report))
    }
}

#[tool_router]
impl RgvMcpServer {
    /// Check whether the configured live RGV window is reachable and protocol-compatible.
    #[tool(
        name = "rgv_status",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn status(&self) -> CallToolResult {
        match self.control.status().await {
            Ok(status)
                if status.schema_version == AUTOMATION_SCHEMA_VERSION
                    && status
                        .control_schema_version
                        .is_none_or(|version| version == AUTOMATION_SCHEMA_VERSION) =>
            {
                success_result(
                    "The live RGV control service is reachable",
                    json!({
                        "connected": true,
                        "compatible": true,
                        "status": status,
                    }),
                )
            }
            Ok(status) => adapter_failure(
                "unsupported_version",
                &format!(
                    "RGV uses control schema {}, but this adapter supports {}",
                    status.schema_version, AUTOMATION_SCHEMA_VERSION
                ),
            ),
            Err(error) => control_failure(&error),
        }
    }

    /// Inspect the current live RGV genome, ordered panels, loci, tracks, readiness, and selection.
    #[tool(
        name = "rgv_get_workspace",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_workspace(&self) -> CallToolResult {
        match self
            .execute_with_data::<AutomationWorkspaceSnapshot>(
                vec![AutomationCommand::DescribeWorkspace],
                0,
                "workspace",
            )
            .await
        {
            Ok((workspace, _)) => success_result(
                "Described the live RGV workspace",
                json!({"workspace": workspace}),
            ),
            Err(error) => error,
        }
    }

    /// Inspect bounded structured evidence for the current semantic selection in the live window.
    #[tool(
        name = "rgv_get_selection",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_selection(&self) -> CallToolResult {
        match self
            .execute_with_data::<Option<rgv_core::AutomationSelectionSnapshot>>(
                vec![AutomationCommand::DescribeSelection],
                0,
                "selection",
            )
            .await
        {
            Ok((selection, _)) => success_result(
                if selection.is_some() {
                    "Described the live RGV selection"
                } else {
                    "The active panel has no semantic selection"
                },
                json!({"selection": selection}),
            ),
            Err(error) => error,
        }
    }

    /// Retrieve the bounded input-to-present timeline for the most recent
    /// vertical scrollbar gesture. Perform a physical drag first, then call
    /// this tool; it does not focus or otherwise mutate the RGV window.
    #[tool(
        name = "rgv_get_interaction_trace",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_interaction_trace(&self) -> CallToolResult {
        match self
            .execute_with_data::<Option<AutomationInteractionTrace>>(
                vec![AutomationCommand::DescribeInteractionTrace],
                0,
                "interaction trace",
            )
            .await
        {
            Ok((trace, _)) => success_result(
                if trace.is_some() {
                    "Retrieved the most recent vertical scrollbar interaction trace"
                } else {
                    "No vertical scrollbar interaction has been recorded"
                },
                json!({"interaction_trace": trace}),
            ),
            Err(error) => error,
        }
    }

    /// Clear all semantic selection state and close the visible inspector.
    #[tool(
        name = "rgv_clear_selection",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn clear_selection(&self) -> CallToolResult {
        self.execute(vec![AutomationCommand::ClearSelection]).await
    }

    /// Explicitly replace the current immutable evidence snapshot with a new revision.
    #[tool(
        name = "rgv_refresh_selection",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn refresh_selection(&self) -> CallToolResult {
        self.execute(vec![AutomationCommand::RefreshSelection])
            .await
    }

    /// List bounded review items without exposing notes or assignments.
    #[tool(
        name = "rgv_get_review_queue",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_review_queue(&self) -> CallToolResult {
        match self
            .execute_with_data::<Value>(
                vec![AutomationCommand::DescribeReviewQueue],
                0,
                "review queue",
            )
            .await
        {
            Ok((queue, _)) => success_result(
                "Described the RGV review queue",
                json!({"review_queue": queue}),
            ),
            Err(error) => error,
        }
    }

    /// Navigate to a stable review item ID or the next pending item.
    #[tool(
        name = "rgv_navigate_review_item",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn navigate_review_item(
        &self,
        Parameters(args): Parameters<ReviewNavigationArgs>,
    ) -> CallToolResult {
        let commands = with_optional_settle(
            vec![AutomationCommand::NavigateReviewItem {
                action: args.action.into_core(),
            }],
            args.wait_until_settled,
            args.timeout_millis,
        );
        self.execute(commands).await
    }

    /// Observe one panel's bounded navigation history without moving its cursor.
    #[tool(
        name = "rgv_get_navigation_history",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_navigation_history(
        &self,
        Parameters(args): Parameters<PanelArgs>,
    ) -> CallToolResult {
        match self
            .execute_with_data::<Value>(
                vec![AutomationCommand::DescribeNavigationHistory {
                    panel_id: PanelId::new(args.panel_id),
                }],
                0,
                "navigation history",
            )
            .await
        {
            Ok((history, _)) => success_result(
                "Described panel navigation history",
                json!({"history": history}),
            ),
            Err(error) => error,
        }
    }

    /// Navigate backward, forward, or to a bounded history index.
    #[tool(
        name = "rgv_navigate_history",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn navigate_history(
        &self,
        Parameters(args): Parameters<HistoryNavigationArgs>,
    ) -> CallToolResult {
        let commands = with_optional_settle(
            vec![AutomationCommand::NavigateHistory {
                panel_id: PanelId::new(args.panel_id),
                action: args.action.into_core(),
            }],
            args.wait_until_settled,
            args.timeout_millis,
        );
        self.execute(commands).await
    }

    /// Find bounded alignment matches and return opaque, generation-scoped handles.
    #[tool(
        name = "rgv_find_alignments",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn find_alignments(
        &self,
        Parameters(args): Parameters<FindAlignmentsArgs>,
    ) -> CallToolResult {
        match self
            .execute_with_data::<Value>(
                vec![AutomationCommand::FindAlignments {
                    panel_id: PanelId::new(args.panel_id),
                    track_id: TrackId::new(args.track_id),
                    selector: args.selector.into_core(),
                    maximum_results: args.maximum_results,
                }],
                0,
                "alignment search",
            )
            .await
        {
            Ok((matches, _)) => success_result(
                "Found bounded alignment matches",
                json!({"matches": matches}),
            ),
            Err(error) => error,
        }
    }

    /// Select one alignment by an opaque handle returned by `rgv_find_alignments`.
    #[tool(
        name = "rgv_select_alignment",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn select_alignment(
        &self,
        Parameters(args): Parameters<AlignmentHandleArgs>,
    ) -> CallToolResult {
        let commands = with_optional_settle(
            vec![AutomationCommand::SelectAlignment {
                handle: args.handle,
                reveal: args.reveal.into_core(),
                expected_workspace_revision: args.expected_workspace_revision,
            }],
            args.wait_until_settled,
            args.timeout_millis,
        );
        self.execute(commands).await
    }

    /// Describe mate and supplementary-chain availability for the selected alignment.
    #[tool(
        name = "rgv_get_related_alignments",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_related_alignments(&self) -> CallToolResult {
        match self
            .execute_with_data::<Value>(
                vec![AutomationCommand::DescribeRelatedAlignments],
                0,
                "related alignments",
            )
            .await
        {
            Ok((related, _)) => {
                success_result("Described related alignments", json!({"related": related}))
            }
            Err(error) => error,
        }
    }

    /// Navigate to a selected alignment's mate or supplementary segment.
    #[tool(
        name = "rgv_navigate_related_alignment",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn navigate_related_alignment(
        &self,
        Parameters(args): Parameters<RelatedNavigationArgs>,
    ) -> CallToolResult {
        let commands = with_optional_settle(
            vec![AutomationCommand::NavigateRelatedAlignment {
                relationship: args.relationship.into_core(),
                segment_index: args.segment_index,
                destination: args.destination.into_core(),
                expected_workspace_revision: args.expected_workspace_revision,
            }],
            args.wait_until_settled,
            args.timeout_millis,
        );
        self.execute(commands).await
    }

    /// Navigate the visible live RGV window to one or more exact one-based genomic loci.
    #[tool(
        name = "rgv_navigate",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn navigate(&self, Parameters(args): Parameters<NavigateArgs>) -> CallToolResult {
        let commands = with_optional_settle(
            vec![AutomationCommand::Goto { loci: args.loci }],
            args.wait_until_settled,
            args.timeout_millis,
        );
        self.execute(commands).await
    }

    /// Pan, zoom, or fit a panel in the visible live RGV window and return its exact interval.
    #[tool(
        name = "rgv_change_viewport",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn change_viewport(
        &self,
        Parameters(args): Parameters<ChangeViewportArgs>,
    ) -> CallToolResult {
        let operation = args.operation.into_core();
        let commands = with_optional_settle(
            vec![AutomationCommand::ChangeViewport {
                panel_id: PanelId::new(args.panel_id),
                operation,
            }],
            args.wait_until_settled,
            args.timeout_millis,
        );
        self.execute(commands).await
    }

    /// Load a track into the visible live RGV window and return its stable track identity.
    #[tool(
        name = "rgv_load_track",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn load_track(&self, Parameters(args): Parameters<LoadTrackArgs>) -> CallToolResult {
        let commands = with_optional_settle(
            vec![AutomationCommand::LoadTrack {
                source: args.source.into_core(),
                kind: args.kind.map(ToolTrackKind::into_core),
                name: args.name,
            }],
            args.wait_until_settled,
            args.timeout_millis,
        );
        self.execute(commands).await
    }

    /// Remove one stable track identity from the visible live RGV window.
    #[tool(
        name = "rgv_remove_track",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn remove_track(&self, Parameters(args): Parameters<TrackIdArgs>) -> CallToolResult {
        let commands = with_optional_settle(
            vec![AutomationCommand::RemoveTrack {
                track_id: TrackId::new(args.track_id),
            }],
            args.wait_until_settled,
            args.timeout_millis,
        );
        self.execute(commands).await
    }

    /// Apply a bounded workspace, track, or panel option to the visible live RGV window.
    #[tool(
        name = "rgv_configure",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn configure(&self, Parameters(args): Parameters<ConfigureArgs>) -> CallToolResult {
        let commands = with_optional_settle(
            vec![AutomationCommand::Configure {
                setting: args.change.into_core(),
            }],
            args.wait_until_settled,
            args.timeout_millis,
        );
        self.execute(commands).await
    }

    /// Group stable alignment tracks in the visible live RGV window.
    #[tool(
        name = "rgv_group_alignments",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn group_alignments(&self, Parameters(args): Parameters<GroupArgs>) -> CallToolResult {
        let Some((mode, tag)) = args.mode.into_core(args.tag.as_deref()) else {
            return adapter_failure(
                "invalid_input",
                "tag grouping requires a two-character SAM tag",
            );
        };
        let commands = with_optional_settle(
            vec![AutomationCommand::Group {
                track_ids: track_ids(&args.track_ids),
                mode,
                direction: args.direction.into_core(),
                tag,
            }],
            args.wait_until_settled,
            args.timeout_millis,
        );
        self.execute(commands).await
    }

    /// Sort stable alignment tracks in the visible live RGV window.
    #[tool(
        name = "rgv_sort_alignments",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn sort_alignments(&self, Parameters(args): Parameters<SortArgs>) -> CallToolResult {
        let Some((mode, tag)) = args.mode.into_core(args.tag.as_deref()) else {
            return adapter_failure(
                "invalid_input",
                "tag sorting requires a two-character SAM tag",
            );
        };
        let commands = with_optional_settle(
            vec![AutomationCommand::Sort {
                track_ids: track_ids(&args.track_ids),
                mode,
                direction: args.direction.into_core(),
                tag,
            }],
            args.wait_until_settled,
            args.timeout_millis,
        );
        self.execute(commands).await
    }

    /// Apply an RGV alignment filter to stable tracks in the visible live RGV window.
    #[tool(
        name = "rgv_filter_alignments",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn filter_alignments(&self, Parameters(args): Parameters<FilterArgs>) -> CallToolResult {
        let commands = with_optional_settle(
            vec![AutomationCommand::Filter {
                track_ids: track_ids(&args.track_ids),
                expression: args.expression,
            }],
            args.wait_until_settled,
            args.timeout_millis,
        );
        self.execute(commands).await
    }

    /// Select a panel, track, or zero-based genomic position in the visible live RGV window.
    #[tool(
        name = "rgv_select",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn select(&self, Parameters(args): Parameters<SelectArgs>) -> CallToolResult {
        let commands = with_optional_settle(
            vec![AutomationCommand::Select {
                selection: args.selection.into_core(),
            }],
            args.wait_until_settled,
            args.timeout_millis,
        );
        self.execute(commands).await
    }

    /// Capture a settled view from the live RGV window and return it as PNG image content.
    #[tool(
        name = "rgv_capture_view",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn capture_view(&self, Parameters(args): Parameters<CaptureArgs>) -> CallToolResult {
        let path = self.artifacts.temporary_path("png");
        let mut commands = Vec::new();
        if args.wait_until_settled {
            commands.push(AutomationCommand::WaitUntilSettled {
                timeout_millis: args.timeout_millis,
            });
        }
        commands.push(AutomationCommand::Snapshot {
            path: path.display().to_string(),
            scope: args.scope.into_core(),
            format: AutomationSnapshotFormat::Png,
        });
        let snapshot_result_index = commands.len() - 1;
        commands.push(AutomationCommand::DescribeWorkspace);
        let report = match self
            .control
            .run_batch(&AutomationBatch {
                schema_version: AUTOMATION_SCHEMA_VERSION,
                commands,
            })
            .await
        {
            Ok(report) if report.succeeded => report,
            Ok(report) => {
                self.artifacts.discard_temporary(&path);
                return report_failure(&report);
            }
            Err(error) => {
                self.artifacts.discard_temporary(&path);
                return control_failure(&error);
            }
        };
        let bytes = match self.artifacts.read_temporary_capture(&path) {
            Ok(bytes) => bytes,
            Err(error) => return artifact_failure(&error),
        };
        let Some((png_width, png_height)) = png_dimensions(&bytes) else {
            return adapter_failure("artifact_error", "RGV returned a malformed PNG capture");
        };
        let capture = report
            .results
            .get(snapshot_result_index)
            .map(|result| &result.data)
            .cloned()
            .unwrap_or(Value::Null);
        if capture.get("width").and_then(Value::as_u64) != Some(u64::from(png_width))
            || capture.get("height").and_then(Value::as_u64) != Some(u64::from(png_height))
        {
            return adapter_failure(
                "invalid_response",
                "RGV capture metadata did not match the PNG dimensions",
            );
        }
        let workspace = report.results.last().and_then(|result| {
            serde_json::from_value::<AutomationWorkspaceSnapshot>(result.data.clone()).ok()
        });
        let encoded = BASE64_STANDARD.encode(&bytes);
        let structured = json!({
            "mime_type": "image/png",
            "bytes": bytes.len(),
            "scope": args.scope,
            "width": png_width,
            "height": png_height,
            "truncated": capture.get("truncated").and_then(Value::as_bool).unwrap_or(false),
            "genome": workspace.as_ref().map(|workspace| &workspace.genome),
            "application_instance_id": workspace.as_ref().map(|workspace| &workspace.application_instance_id),
            "workspace_revision": workspace.as_ref().map(|workspace| workspace.workspace_revision),
            "selection_id": workspace.as_ref().and_then(|workspace| workspace.selection.as_ref()).map(|selection| &selection.selection_id),
            "evidence_revision": workspace.as_ref().and_then(|workspace| workspace.selection.as_ref()).map(|selection| selection.evidence_revision),
            "panels": workspace.as_ref().map(|workspace| &workspace.panels),
            "loci": workspace.as_ref().map(|workspace| {
                workspace.panels.iter().map(|panel| panel.locus.as_str()).collect::<Vec<_>>()
            }).unwrap_or_default(),
            "report": public_report(&report),
        });
        let mut result = CallToolResult::success(vec![
            ContentBlock::text("Captured the settled live RGV view"),
            ContentBlock::image(encoded, "image/png"),
        ]);
        result.structured_content = Some(structured);
        result
    }

    /// Capture the native RGV window client area without activating or focusing it.
    #[tool(
        name = "rgv_capture_window",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn capture_window(
        &self,
        Parameters(args): Parameters<CaptureWindowArgs>,
    ) -> CallToolResult {
        let path = self.artifacts.temporary_path("png");
        let mut commands = Vec::new();
        if args.wait_until_settled {
            commands.push(AutomationCommand::WaitUntilSettled {
                timeout_millis: args.timeout_millis,
            });
        }
        let snapshot_result_index = commands.len();
        commands.push(AutomationCommand::Snapshot {
            path: path.display().to_string(),
            scope: AutomationSnapshotScope::NativeWindow,
            format: AutomationSnapshotFormat::Png,
        });
        commands.push(AutomationCommand::DescribeWorkspace);
        let report = match self
            .control
            .run_batch(&AutomationBatch {
                schema_version: AUTOMATION_SCHEMA_VERSION,
                commands,
            })
            .await
        {
            Ok(report) if report.succeeded => report,
            Ok(report) => {
                self.artifacts.discard_temporary(&path);
                return report_failure(&report);
            }
            Err(error) => {
                self.artifacts.discard_temporary(&path);
                return control_failure(&error);
            }
        };
        let workspace = report.results.last().and_then(|result| {
            serde_json::from_value::<AutomationWorkspaceSnapshot>(result.data.clone()).ok()
        });
        let Some(workspace) = workspace else {
            self.artifacts.discard_temporary(&path);
            return adapter_failure(
                "invalid_response",
                "RGV omitted the workspace associated with its window capture",
            );
        };
        let status = match self.control.status().await {
            Ok(status) => status,
            Err(error) => {
                self.artifacts.discard_temporary(&path);
                return control_failure(&error);
            }
        };
        if status.schema_version != AUTOMATION_SCHEMA_VERSION
            || status
                .control_schema_version
                .is_some_and(|version| version != AUTOMATION_SCHEMA_VERSION)
        {
            return adapter_failure(
                "unsupported_version",
                "RGV changed control protocol before native window capture",
            );
        }
        if status.process_id == 0
            || status.application_instance_id.is_empty()
            || status.application_instance_id != workspace.application_instance_id
        {
            return adapter_failure(
                "application_identity_mismatch",
                "RGV process identity could not be bound to the authenticated workspace",
            );
        }
        let bytes = match self.artifacts.read_temporary_capture(&path) {
            Ok(bytes) => bytes,
            Err(error) => return artifact_failure(&error),
        };
        let Some((png_width, png_height)) = png_dimensions(&bytes) else {
            return adapter_failure("artifact_error", "RGV returned a malformed window PNG");
        };
        let capture = report
            .results
            .get(snapshot_result_index)
            .map(|result| &result.data)
            .cloned()
            .unwrap_or(Value::Null);
        if capture.get("width").and_then(Value::as_u64) != Some(u64::from(png_width))
            || capture.get("height").and_then(Value::as_u64) != Some(u64::from(png_height))
        {
            return adapter_failure(
                "invalid_response",
                "RGV window capture metadata did not match the PNG dimensions",
            );
        }
        let encoded = BASE64_STANDARD.encode(&bytes);
        let loci = workspace
            .panels
            .iter()
            .map(|panel| panel.locus.as_str())
            .collect::<Vec<_>>();
        let structured = json!({
            "mime_type": "image/png",
            "bytes": bytes.len(),
            "capture_kind": "native_window_client_area",
            "platform": std::env::consts::OS,
            "width": png_width,
            "height": png_height,
            "process_id": status.process_id,
            "includes_native_frame": false,
            "foreground_activated": false,
            "application_instance_id": &workspace.application_instance_id,
            "workspace_revision": workspace.workspace_revision,
            "selection_id": workspace.selection.as_ref().map(|selection| &selection.selection_id),
            "evidence_revision": workspace.selection.as_ref().map(|selection| selection.evidence_revision),
            "panels": &workspace.panels,
            "loci": loci,
            "report": public_report(&report),
        });
        let mut result = CallToolResult::success(vec![
            ContentBlock::text("Captured the native RGV window client area without activating it"),
            ContentBlock::image(encoded, "image/png"),
        ]);
        result.structured_content = Some(structured);
        result
    }

    /// Export bounded genomic evidence from the live RGV window to a control-root-relative artifact.
    #[tool(
        name = "rgv_export",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn export(&self, Parameters(args): Parameters<ExportArgs>) -> CallToolResult {
        let path = match self.artifacts.durable_path(Path::new(&args.path)) {
            Ok(path) => path,
            Err(error) => return artifact_failure(&error),
        };
        let mut commands = Vec::new();
        if args.wait_until_settled {
            commands.push(AutomationCommand::WaitUntilSettled {
                timeout_millis: args.timeout_millis,
            });
        }
        commands.push(AutomationCommand::Export {
            path: path.display().to_string(),
            kind: args.kind.into_core(),
        });
        let report = match self
            .control
            .run_batch(&AutomationBatch {
                schema_version: AUTOMATION_SCHEMA_VERSION,
                commands,
            })
            .await
        {
            Ok(report) if report.succeeded => report,
            Ok(report) => return report_failure(&report),
            Err(error) => return control_failure(&error),
        };
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => return adapter_failure("artifact_error", "RGV export is not a regular file"),
            Err(error) => return artifact_failure(&ArtifactError::Io(error)),
        };
        let relative = match self.artifacts.relative_display(&path) {
            Ok(path) => path,
            Err(error) => return artifact_failure(&error),
        };
        let inline_text = match self
            .artifacts
            .read_durable_text(&path, MAXIMUM_INLINE_EXPORT_BYTES)
        {
            Ok(value) => value,
            Err(error) => return artifact_failure(&error),
        };
        success_result(
            "Exported bounded evidence from the live RGV window",
            json!({
                "kind": args.kind,
                "path": relative,
                "bytes": metadata.len(),
                "inline_text": inline_text,
                "report": public_report(&report),
            }),
        )
    }

    /// Save the current live RGV workspace under the configured control root.
    #[tool(
        name = "rgv_save_workspace",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn save_workspace(&self, Parameters(args): Parameters<SaveArgs>) -> CallToolResult {
        let path = match self.artifacts.durable_path(Path::new(&args.path)) {
            Ok(path) => path,
            Err(error) => return artifact_failure(&error),
        };
        let report = match self
            .control
            .run_batch(&AutomationBatch {
                schema_version: AUTOMATION_SCHEMA_VERSION,
                commands: vec![AutomationCommand::Save {
                    path: path.display().to_string(),
                }],
            })
            .await
        {
            Ok(report) if report.succeeded => report,
            Ok(report) => return report_failure(&report),
            Err(error) => return control_failure(&error),
        };
        let relative = match self.artifacts.relative_display(&path) {
            Ok(path) => path,
            Err(error) => return artifact_failure(&error),
        };
        success_result(
            "Saved the live RGV workspace",
            json!({"path": relative, "report": public_report(&report)}),
        )
    }

    /// Open a control-root-confined project, explicitly replacing the visible workspace.
    #[tool(
        name = "rgv_open_workspace",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn open_workspace(
        &self,
        Parameters(args): Parameters<OpenWorkspaceArgs>,
    ) -> CallToolResult {
        if !args.confirm_replace {
            return adapter_failure(
                "workspace_replacement_not_confirmed",
                "confirm_replace must be true to replace the visible workspace",
            );
        }
        let path = match self.artifacts.durable_path(Path::new(&args.path)) {
            Ok(path) if path.is_file() => path,
            Ok(_) => {
                return adapter_failure(
                    "artifact_error",
                    "workspace project is not a regular file",
                );
            }
            Err(error) => return artifact_failure(&error),
        };
        let commands = with_optional_settle(
            vec![AutomationCommand::OpenWorkspace {
                path: path.display().to_string(),
                confirm_replace: true,
                unsaved_changes: args.unsaved_changes.into_core(),
                timeout_millis: args.timeout_millis,
            }],
            args.wait_until_settled,
            args.timeout_millis,
        );
        self.execute(commands).await
    }

    /// Execute an advanced bounded sequence that may visibly mutate the live RGV window.
    #[tool(
        name = "rgv_execute_batch",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn execute_batch(
        &self,
        Parameters(args): Parameters<ExecuteBatchArgs>,
    ) -> CallToolResult {
        if args.commands.is_empty() || args.commands.len() > MAXIMUM_MCP_BATCH_COMMANDS {
            return adapter_failure(
                "invalid_input",
                "commands must contain between 1 and 100 entries",
            );
        }
        let value = json!({
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "commands": args.commands,
        });
        let Ok(batch) = AutomationBatch::from_json(&value.to_string()) else {
            return adapter_failure(
                "invalid_batch",
                "commands do not form a valid RGV automation batch",
            );
        };
        if batch
            .commands
            .iter()
            .any(|command| matches!(command, AutomationCommand::Exit))
        {
            return adapter_failure("forbidden_command", "exit is not available through MCP");
        }
        if !args.allow_destructive
            && batch
                .commands
                .iter()
                .any(|command| matches!(command, AutomationCommand::NewWorkspace))
        {
            return adapter_failure(
                "destructive_confirmation_required",
                "new_workspace requires allow_destructive=true",
            );
        }
        match self.control.run_batch(&batch).await {
            Ok(report) if report.succeeded => success_result(
                "RGV completed the advanced batch",
                json!({"report": public_report(&report)}),
            ),
            Ok(report) => report_failure(&report),
            Err(error) => control_failure(&error),
        }
    }
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != PNG_SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

#[tool_handler]
impl ServerHandler for RgvMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("rgv-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Inspect the live RGV workspace before mutating it. Mutation tools visibly change the attached RGV desktop window. Genomic loci are one-based strings; explicit position and interval fields are zero-based, half-open coordinates.",
            )
    }
}

fn with_optional_settle(
    mut commands: Vec<AutomationCommand>,
    wait_until_settled: bool,
    timeout_millis: u64,
) -> Vec<AutomationCommand> {
    if wait_until_settled {
        commands.push(AutomationCommand::WaitUntilSettled { timeout_millis });
    }
    commands
}

fn track_ids(ids: &[u64]) -> Vec<TrackId> {
    ids.iter().copied().map(TrackId::new).collect()
}

fn success_result(summary: &str, value: Value) -> CallToolResult {
    let mut result = CallToolResult::success(vec![ContentBlock::text(summary.to_owned())]);
    result.structured_content = Some(value);
    result
}

fn adapter_failure(code: &str, message: &str) -> CallToolResult {
    let value = json!({"error": {"code": code, "message": message}});
    let mut result = CallToolResult::error(vec![ContentBlock::text(message.to_owned())]);
    result.structured_content = Some(value);
    result
}

fn control_failure(error: &ControlClientError) -> CallToolResult {
    let (code, message) = match error {
        ControlClientError::Transport(_) => (
            "unavailable",
            "The configured RGV window is unavailable; start it with local control enabled and retry".to_owned(),
        ),
        ControlClientError::Http { code, .. } => (
            code.as_str(),
            format!("RGV local control rejected the request ({code})"),
        ),
        ControlClientError::InvalidBatch(_) => ("invalid_batch", "The generated RGV batch was invalid".to_owned()),
        ControlClientError::Json(_) | ControlClientError::InvalidResponse(_) => (
            "invalid_response",
            "RGV local control returned malformed JSON".to_owned(),
        ),
        ControlClientError::ResponseTooLarge => (
            "response_too_large",
            "RGV local control exceeded the adapter response limit".to_owned(),
        ),
        ControlClientError::UnexpectedContentType => (
            "invalid_response",
            "RGV local control returned an unexpected content type".to_owned(),
        ),
        ControlClientError::StateReobservationFailed => (
            "state_reobservation_failed",
            "RGV state could not be re-observed after an interrupted mutation".to_owned(),
        ),
    };
    adapter_failure(code, &message)
}

fn artifact_failure(error: &ArtifactError) -> CallToolResult {
    adapter_failure("artifact_error", &error.to_string())
}

fn report_failure(report: &AutomationRunReport) -> CallToolResult {
    let failed = report
        .results
        .iter()
        .find(|result| result.status == AutomationCommandStatus::Failed);
    let code =
        failed
            .and_then(|result| result.error_code)
            .map_or("viewer_failed".to_owned(), |code| {
                serde_json::to_value(code)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "viewer_failed".to_owned())
            });
    let message = failed.map_or_else(
        || "RGV did not complete the requested operation".to_owned(),
        |result| format!("RGV command {} failed", result.command),
    );
    let mut result = CallToolResult::error(vec![ContentBlock::text(message.clone())]);
    result.structured_content = Some(json!({
        "error": {"code": code, "message": message},
        "report": public_report(report),
    }));
    result
}

fn public_report(report: &AutomationRunReport) -> Value {
    json!({
        "schema_version": report.schema_version,
        "succeeded": report.succeeded,
        "results": report.results.iter().map(|result| json!({
            "command_index": result.command_index,
            "command": result.command,
            "status": result.status,
            "error_code": result.error_code,
            "message": match result.status {
                AutomationCommandStatus::Succeeded => "Command succeeded",
                AutomationCommandStatus::Failed => "Command failed",
                AutomationCommandStatus::Skipped => "Command skipped",
            },
            "data": public_data(&result.data),
            "elapsed_ms": result.elapsed_ms,
        })).collect::<Vec<_>>(),
        "telemetry": report.telemetry,
    })
}

fn public_data(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    let sensitive_identity = matches!(
                        lower.as_str(),
                        "read_name" | "read_sequence" | "sequence" | "notes" | "assignment"
                    );
                    let value = if lower.contains("path")
                        || lower.contains("url")
                        || lower.contains("source")
                        || sensitive_identity
                    {
                        Value::String("[confined or redacted]".to_owned())
                    } else {
                        public_data(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(public_data).collect()),
        value => value.clone(),
    }
}

fn default_true() -> bool {
    true
}

fn default_timeout() -> u64 {
    DEFAULT_SETTLE_TIMEOUT_MILLIS
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NavigateArgs {
    #[schemars(length(min = 1, max = 16))]
    pub loci: Vec<String>,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChangeViewportArgs {
    #[schemars(range(min = 1))]
    pub panel_id: u64,
    pub operation: ToolViewportOperation,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ToolViewportOperation {
    PanBases {
        delta: i64,
    },
    PanFraction {
        #[schemars(range(min = -100.0, max = 100.0))]
        fraction: f64,
    },
    Zoom {
        #[schemars(range(min = 0.01, max = 100.0))]
        factor: f64,
        anchor_position: Option<u64>,
    },
    FitInterval {
        start: u64,
        end: u64,
    },
}

impl ToolViewportOperation {
    fn into_core(self) -> AutomationViewportOperation {
        match self {
            Self::PanBases { delta } => AutomationViewportOperation::PanBases { delta },
            Self::PanFraction { fraction } => AutomationViewportOperation::PanFraction { fraction },
            Self::Zoom {
                factor,
                anchor_position,
            } => AutomationViewportOperation::Zoom {
                factor,
                anchor_position,
            },
            Self::FitInterval { start, end } => {
                AutomationViewportOperation::FitInterval { start, end }
            }
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LoadTrackArgs {
    pub source: ToolSource,
    pub kind: Option<ToolTrackKind>,
    pub name: Option<String>,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolSource {
    Local { path: String },
    Remote { url: String },
}

impl ToolSource {
    fn into_core(self) -> TrackSource {
        match self {
            Self::Local { path } => TrackSource::Local { path },
            Self::Remote { url } => TrackSource::Remote { url },
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolTrackKind {
    Alignment,
    Reference,
    Feature,
    GeneModel,
    Variant,
    Mutation,
    Gwas,
    Quantitative,
    CopyNumber,
    Interaction,
}

impl ToolTrackKind {
    fn into_core(self) -> TrackKind {
        match self {
            Self::Alignment => TrackKind::Alignment,
            Self::Reference => TrackKind::Reference,
            Self::Feature => TrackKind::Feature,
            Self::GeneModel => TrackKind::GeneModel,
            Self::Variant => TrackKind::Variant,
            Self::Mutation => TrackKind::Mutation,
            Self::Gwas => TrackKind::Gwas,
            Self::Quantitative => TrackKind::Quantitative,
            Self::CopyNumber => TrackKind::CopyNumber,
            Self::Interaction => TrackKind::Interaction,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TrackIdArgs {
    #[schemars(range(min = 1))]
    pub track_id: u64,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfigureArgs {
    pub change: ToolSetting,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolTrackChannel {
    Annotation,
    Reads,
}

impl ToolTrackChannel {
    const fn into_core(self) -> TrackChannel {
        match self {
            Self::Annotation => TrackChannel::Annotation,
            Self::Reads => TrackChannel::Reads,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolPresentationNode {
    Reference,
    Track {
        channel: ToolTrackChannel,
        #[schemars(range(min = 1))]
        track_id: u64,
    },
    Group {
        channel: ToolTrackChannel,
        #[schemars(range(min = 1))]
        group_id: u32,
    },
}

impl ToolPresentationNode {
    const fn into_core(self) -> PresentationNodeId {
        match self {
            Self::Reference => PresentationNodeId::Reference,
            Self::Track { channel, track_id } => PresentationNodeId::Track {
                channel: channel.into_core(),
                track_id: TrackId::new(track_id),
            },
            Self::Group { channel, group_id } => PresentationNodeId::Group {
                channel: channel.into_core(),
                group_id,
            },
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "setting", rename_all = "snake_case")]
pub enum ToolSetting {
    WindowSize {
        #[schemars(range(min = 640, max = 3840))]
        width: u32,
        #[schemars(range(min = 400, max = 2160))]
        height: u32,
    },
    InterfaceScalePercent {
        #[schemars(range(min = 75, max = 200))]
        value: u16,
    },
    PropertiesPanelExpanded {
        value: bool,
    },
    PropertiesPanelWidth {
        #[schemars(range(min = 280.0, max = 520.0))]
        value: f32,
    },
    ShowReference {
        value: bool,
    },
    ShowCoverage {
        value: bool,
    },
    ShowReads {
        value: bool,
    },
    ShowJunctions {
        value: bool,
    },
    ViewAsPairs {
        value: bool,
    },
    OverlappingMatePolicy {
        value: ToolOverlappingMatePolicy,
    },
    AlignmentColorMode {
        #[schemars(length(min = 1, max = 1024))]
        track_ids: Vec<u64>,
        value: ToolAlignmentColorMode,
    },
    AlignmentColorTag {
        #[schemars(length(min = 1, max = 1024))]
        track_ids: Vec<u64>,
        #[schemars(length(min = 2, max = 2))]
        #[schemars(regex(pattern = r"^[A-Za-z][A-Za-z0-9]$"))]
        tag: String,
    },
    AlignmentDensity {
        #[schemars(range(min = 0.25, max = 4.0))]
        value: f32,
    },
    IncludeSecondary {
        value: bool,
    },
    IncludeSupplementary {
        value: bool,
    },
    CoverageScaleTransform {
        value: ToolCoverageScaleTransform,
    },
    ReadBaseDisplayMode {
        value: ToolReadBaseDisplayMode,
    },
    ShadeReadBasesByQuality {
        value: bool,
    },
    ShowModifiedBases {
        value: bool,
    },
    ModifiedBaseColorMode {
        value: ToolModifiedBaseColorMode,
    },
    PresentationNodeVisible {
        target: ToolPresentationNode,
        value: bool,
    },
    PresentationNodeHeight {
        target: ToolPresentationNode,
        #[schemars(range(min = 26.0, max = 1200.0))]
        value: f32,
    },
    PresentationGroupExpanded {
        target: ToolPresentationNode,
        value: bool,
    },
    PresentationGroupVisible {
        target: ToolPresentationNode,
        value: bool,
    },
    PresentationCompositeVisible {
        channel: ToolTrackChannel,
        #[schemars(range(min = 1))]
        composite_id: u32,
        value: bool,
    },
    PresentationGroup {
        channel: ToolTrackChannel,
        #[schemars(length(min = 2, max = 1024))]
        track_ids: Vec<u64>,
    },
    PresentationUngroup {
        target: ToolPresentationNode,
    },
    ReadPaneHeight {
        #[schemars(range(min = 100.0, max = 1200.0))]
        value: f32,
    },
    TrackHeight {
        #[schemars(range(min = 1))]
        track_id: u64,
        #[schemars(range(min = 40.0, max = 1200.0))]
        value: f32,
    },
    TrackCollapsed {
        #[schemars(range(min = 1))]
        track_id: u64,
        value: bool,
    },
    TrackVisible {
        #[schemars(range(min = 1))]
        track_id: u64,
        value: bool,
    },
    VerticalScrollPoints {
        #[schemars(range(min = 1))]
        panel_id: u64,
        #[schemars(range(min = 0.0))]
        value: f32,
    },
    AnnotationScrollPoints {
        #[schemars(range(min = 1))]
        panel_id: u64,
        #[schemars(range(min = 0.0))]
        value: f32,
    },
    ReadStackScrollPoints {
        #[schemars(range(min = 1))]
        panel_id: u64,
        #[schemars(range(min = 0.0))]
        value: f32,
    },
    ReadPanelScrollPoints {
        #[schemars(range(min = 1))]
        panel_id: u64,
        #[schemars(range(min = 1))]
        track_id: u64,
        #[schemars(range(min = 0.0))]
        value: f32,
    },
    ReadPanelDensity {
        #[schemars(range(min = 1))]
        panel_id: u64,
        #[schemars(range(min = 1))]
        track_id: u64,
        #[schemars(range(min = 0.25, max = 4.0))]
        value: f32,
    },
}

impl ToolSetting {
    fn into_core(self) -> AutomationSetting {
        match self {
            Self::WindowSize { width, height } => AutomationSetting::WindowSize { width, height },
            Self::InterfaceScalePercent { value } => {
                AutomationSetting::InterfaceScalePercent { value }
            }
            Self::PropertiesPanelExpanded { value } => {
                AutomationSetting::PropertiesPanelExpanded { value }
            }
            Self::PropertiesPanelWidth { value } => {
                AutomationSetting::PropertiesPanelWidth { value }
            }
            Self::ShowReference { value } => AutomationSetting::ShowReference { value },
            Self::ShowCoverage { value } => AutomationSetting::ShowCoverage { value },
            Self::ShowReads { value } => AutomationSetting::ShowReads { value },
            Self::ShowJunctions { value } => AutomationSetting::ShowJunctions { value },
            Self::ViewAsPairs { value } => AutomationSetting::ViewAsPairs { value },
            Self::OverlappingMatePolicy { value } => AutomationSetting::OverlappingMatePolicy {
                value: value.into_core(),
            },
            Self::AlignmentColorMode {
                track_ids: ids,
                value,
            } => AutomationSetting::AlignmentColorMode {
                track_ids: track_ids(&ids),
                value: value.into_core(),
            },
            Self::AlignmentColorTag {
                track_ids: ids,
                tag,
            } => {
                let bytes = tag.as_bytes();
                debug_assert_eq!(bytes.len(), 2, "the MCP schema constrains SAM tags");
                let tag = [
                    bytes.first().copied().unwrap_or_default(),
                    bytes.get(1).copied().unwrap_or_default(),
                ];
                AutomationSetting::AlignmentColorMode {
                    track_ids: track_ids(&ids),
                    value: AlignmentColorMode::Tag(tag),
                }
            }
            Self::AlignmentDensity { value } => AutomationSetting::AlignmentDensity { value },
            Self::IncludeSecondary { value } => AutomationSetting::IncludeSecondary { value },
            Self::IncludeSupplementary { value } => {
                AutomationSetting::IncludeSupplementary { value }
            }
            Self::CoverageScaleTransform { value } => AutomationSetting::CoverageScaleTransform {
                value: value.into_core(),
            },
            Self::ReadBaseDisplayMode { value } => AutomationSetting::ReadBaseDisplayMode {
                value: value.into_core(),
            },
            Self::ShadeReadBasesByQuality { value } => {
                AutomationSetting::ShadeReadBasesByQuality { value }
            }
            Self::ShowModifiedBases { value } => AutomationSetting::ShowModifiedBases { value },
            Self::ModifiedBaseColorMode { value } => AutomationSetting::ModifiedBaseColorMode {
                value: value.into_core(),
            },
            Self::PresentationNodeVisible { target, value } => {
                AutomationSetting::PresentationNodeVisible {
                    target: target.into_core(),
                    value,
                }
            }
            Self::PresentationNodeHeight { target, value } => {
                AutomationSetting::PresentationNodeHeight {
                    target: target.into_core(),
                    value,
                }
            }
            Self::PresentationGroupExpanded { target, value } => {
                AutomationSetting::PresentationGroupExpanded {
                    target: target.into_core(),
                    value,
                }
            }
            Self::PresentationGroupVisible { target, value } => {
                AutomationSetting::PresentationGroupVisible {
                    target: target.into_core(),
                    value,
                }
            }
            Self::PresentationCompositeVisible {
                channel,
                composite_id,
                value,
            } => AutomationSetting::PresentationCompositeVisible {
                channel: channel.into_core(),
                composite_id,
                value,
            },
            Self::PresentationGroup { channel, track_ids } => {
                AutomationSetting::PresentationGroup {
                    channel: channel.into_core(),
                    track_ids: track_ids.into_iter().map(TrackId::new).collect(),
                }
            }
            Self::PresentationUngroup { target } => AutomationSetting::PresentationUngroup {
                target: target.into_core(),
            },
            Self::ReadPaneHeight { value } => AutomationSetting::ReadPaneHeight { value },
            Self::TrackHeight { track_id, value } => AutomationSetting::TrackHeight {
                track_id: TrackId::new(track_id),
                value,
            },
            Self::TrackCollapsed { track_id, value } => AutomationSetting::TrackCollapsed {
                track_id: TrackId::new(track_id),
                value,
            },
            Self::TrackVisible { track_id, value } => AutomationSetting::TrackVisible {
                track_id: TrackId::new(track_id),
                value,
            },
            Self::VerticalScrollPoints { panel_id, value } => {
                AutomationSetting::VerticalScrollPoints {
                    panel_id: PanelId::new(panel_id),
                    value,
                }
            }
            Self::AnnotationScrollPoints { panel_id, value } => {
                AutomationSetting::AnnotationScrollPoints {
                    panel_id: PanelId::new(panel_id),
                    value,
                }
            }
            Self::ReadStackScrollPoints { panel_id, value } => {
                AutomationSetting::ReadStackScrollPoints {
                    panel_id: PanelId::new(panel_id),
                    value,
                }
            }
            Self::ReadPanelScrollPoints {
                panel_id,
                track_id,
                value,
            } => AutomationSetting::ReadPanelScrollPoints {
                panel_id: PanelId::new(panel_id),
                track_id: TrackId::new(track_id),
                value,
            },
            Self::ReadPanelDensity {
                panel_id,
                track_id,
                value,
            } => AutomationSetting::ReadPanelDensity {
                panel_id: PanelId::new(panel_id),
                track_id: TrackId::new(track_id),
                value,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolCoverageScaleTransform {
    Linear,
    SquareRoot,
}

impl ToolCoverageScaleTransform {
    fn into_core(self) -> CoverageScaleTransform {
        match self {
            Self::Linear => CoverageScaleTransform::Linear,
            Self::SquareRoot => CoverageScaleTransform::SquareRoot,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolOverlappingMatePolicy {
    CountOnce,
    CountBoth,
}
impl ToolOverlappingMatePolicy {
    fn into_core(self) -> OverlappingMatePolicy {
        match self {
            Self::CountOnce => OverlappingMatePolicy::CountOnce,
            Self::CountBoth => OverlappingMatePolicy::CountBoth,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolReadBaseDisplayMode {
    Mismatches,
    All,
    Hidden,
}
impl ToolReadBaseDisplayMode {
    fn into_core(self) -> ReadBaseDisplayMode {
        match self {
            Self::Mismatches => ReadBaseDisplayMode::Mismatches,
            Self::All => ReadBaseDisplayMode::All,
            Self::Hidden => ReadBaseDisplayMode::Hidden,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolModifiedBaseColorMode {
    ByType,
    Mono,
    ModifiedVsUnmodified,
}
impl ToolModifiedBaseColorMode {
    fn into_core(self) -> ModifiedBaseColorMode {
        match self {
            Self::ByType => ModifiedBaseColorMode::ByType,
            Self::Mono => ModifiedBaseColorMode::Mono,
            Self::ModifiedVsUnmodified => ModifiedBaseColorMode::ModifiedVsUnmodified,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolAlignmentColorMode {
    None,
    Strand,
    MappingQuality,
    ReadGroup,
    Haplotype,
    TemplateLength,
    PairOrientation,
    UnexpectedPair,
    MateContig,
    FirstOfPairStrand,
}
impl ToolAlignmentColorMode {
    fn into_core(self) -> AlignmentColorMode {
        match self {
            Self::None => AlignmentColorMode::None,
            Self::Strand => AlignmentColorMode::Strand,
            Self::MappingQuality => AlignmentColorMode::MappingQuality,
            Self::ReadGroup => AlignmentColorMode::ReadGroup,
            Self::Haplotype => AlignmentColorMode::Haplotype,
            Self::TemplateLength => AlignmentColorMode::TemplateLength,
            Self::PairOrientation => AlignmentColorMode::PairOrientation,
            Self::UnexpectedPair => AlignmentColorMode::UnexpectedPair,
            Self::MateContig => AlignmentColorMode::MateContig,
            Self::FirstOfPairStrand => AlignmentColorMode::FirstOfPairStrand,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GroupArgs {
    #[schemars(length(min = 1, max = 1024))]
    pub track_ids: Vec<u64>,
    pub mode: ToolGroupMode,
    #[serde(default)]
    pub direction: ToolDirection,
    #[schemars(regex(pattern = r"^[A-Za-z][A-Za-z0-9]$"))]
    pub tag: Option<String>,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolGroupMode {
    None,
    Strand,
    ReadGroup,
    Haplotype,
    Chimeric,
    Supplementary,
    LeftClip,
    RightClip,
    PairOrientation,
    UnexpectedPair,
    MateContig,
    ReadOrder,
    FirstOfPairStrand,
    Tag,
}

impl ToolGroupMode {
    fn into_core(self, tag: Option<&str>) -> Option<(AlignmentGroupMode, Option<String>)> {
        let mode = match self {
            Self::None => AlignmentGroupMode::None,
            Self::Strand => AlignmentGroupMode::Strand,
            Self::ReadGroup => AlignmentGroupMode::ReadGroup,
            Self::Haplotype => AlignmentGroupMode::Haplotype,
            Self::Chimeric => AlignmentGroupMode::Chimeric,
            Self::Supplementary => AlignmentGroupMode::Supplementary,
            Self::LeftClip => AlignmentGroupMode::LeftClip,
            Self::RightClip => AlignmentGroupMode::RightClip,
            Self::PairOrientation => AlignmentGroupMode::PairOrientation,
            Self::UnexpectedPair => AlignmentGroupMode::UnexpectedPair,
            Self::MateContig => AlignmentGroupMode::MateContig,
            Self::ReadOrder => AlignmentGroupMode::ReadOrder,
            Self::FirstOfPairStrand => AlignmentGroupMode::FirstOfPairStrand,
            Self::Tag => {
                let tag = valid_tag(tag?)?;
                return Some((AlignmentGroupMode::None, Some(tag)));
            }
        };
        Some((mode, None))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SortArgs {
    #[schemars(length(min = 1, max = 1024))]
    pub track_ids: Vec<u64>,
    pub mode: ToolSortMode,
    #[serde(default)]
    pub direction: ToolDirection,
    #[schemars(regex(pattern = r"^[A-Za-z][A-Za-z0-9]$"))]
    pub tag: Option<String>,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolSortMode {
    Start,
    Strand,
    FirstOfPairStrand,
    MappingQuality,
    ReadName,
    ReadGroup,
    Haplotype,
    AlignedLength,
    LeftClip,
    RightClip,
    TemplateLength,
    PairOrientation,
    MateContig,
    PairStatus,
    ReadOrder,
    Base,
    Tag,
}

impl ToolSortMode {
    fn into_core(self, tag: Option<&str>) -> Option<(AlignmentSortMode, Option<String>)> {
        let mode = match self {
            Self::Start => AlignmentSortMode::Start,
            Self::Strand => AlignmentSortMode::Strand,
            Self::FirstOfPairStrand => AlignmentSortMode::FirstOfPairStrand,
            Self::MappingQuality => AlignmentSortMode::MappingQuality,
            Self::ReadName => AlignmentSortMode::ReadName,
            Self::ReadGroup => AlignmentSortMode::ReadGroup,
            Self::Haplotype => AlignmentSortMode::Haplotype,
            Self::AlignedLength => AlignmentSortMode::AlignedLength,
            Self::LeftClip => AlignmentSortMode::LeftClip,
            Self::RightClip => AlignmentSortMode::RightClip,
            Self::TemplateLength => AlignmentSortMode::TemplateLength,
            Self::PairOrientation => AlignmentSortMode::PairOrientation,
            Self::MateContig => AlignmentSortMode::MateContig,
            Self::PairStatus => AlignmentSortMode::PairStatus,
            Self::ReadOrder => AlignmentSortMode::ReadOrder,
            Self::Base => AlignmentSortMode::Base,
            Self::Tag => {
                let tag = valid_tag(tag?)?;
                return Some((AlignmentSortMode::Start, Some(tag)));
            }
        };
        Some((mode, None))
    }
}

fn valid_tag(tag: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    (bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1].is_ascii_alphanumeric())
        .then(|| tag.to_owned())
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolDirection {
    #[default]
    Natural,
    Reverse,
}

impl ToolDirection {
    fn into_core(self) -> AlignmentOrderDirection {
        match self {
            Self::Natural => AlignmentOrderDirection::Natural,
            Self::Reverse => AlignmentOrderDirection::Reverse,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FilterArgs {
    #[schemars(length(min = 1, max = 1024))]
    pub track_ids: Vec<u64>,
    pub expression: String,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SelectArgs {
    pub selection: ToolSelection,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "selection", rename_all = "snake_case")]
pub enum ToolSelection {
    Track {
        #[schemars(range(min = 1))]
        track_id: u64,
    },
    Panel {
        #[schemars(range(min = 1))]
        panel_id: u64,
    },
    Position {
        #[schemars(range(min = 1))]
        panel_id: u64,
        #[schemars(range(min = 1))]
        track_id: u64,
        position: u64,
        #[serde(default)]
        granularity: ToolSelectionGranularity,
    },
}

impl ToolSelection {
    fn into_core(self) -> AutomationSelection {
        match self {
            Self::Track { track_id } => AutomationSelection::Track {
                track_id: TrackId::new(track_id),
            },
            Self::Panel { panel_id } => AutomationSelection::Panel {
                panel_id: PanelId::new(panel_id),
            },
            Self::Position {
                panel_id,
                track_id,
                position,
                granularity,
            } => AutomationSelection::Position {
                panel_id: PanelId::new(panel_id),
                track_id: TrackId::new(track_id),
                position,
                granularity: granularity.into_core(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolSelectionGranularity {
    #[default]
    ExactBase,
    VisibleBin,
}

impl ToolSelectionGranularity {
    fn into_core(self) -> AutomationSelectionGranularity {
        match self {
            Self::ExactBase => AutomationSelectionGranularity::ExactBase,
            Self::VisibleBin => AutomationSelectionGranularity::VisibleBin,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PanelArgs {
    #[schemars(range(min = 1))]
    pub panel_id: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReviewNavigationArgs {
    pub action: ToolReviewNavigation,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ToolReviewNavigation {
    Item {
        #[schemars(range(min = 1))]
        review_item_id: u64,
    },
    NextPending,
}
impl ToolReviewNavigation {
    fn into_core(self) -> AutomationReviewNavigation {
        match self {
            Self::Item { review_item_id } => AutomationReviewNavigation::Item { review_item_id },
            Self::NextPending => AutomationReviewNavigation::NextPending,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HistoryNavigationArgs {
    #[schemars(range(min = 1))]
    pub panel_id: u64,
    pub action: ToolHistoryNavigation,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ToolHistoryNavigation {
    Back,
    Forward,
    Index { index: usize },
}
impl ToolHistoryNavigation {
    fn into_core(self) -> AutomationHistoryNavigation {
        match self {
            Self::Back => AutomationHistoryNavigation::Back,
            Self::Forward => AutomationHistoryNavigation::Forward,
            Self::Index { index } => AutomationHistoryNavigation::Index { index },
        }
    }
}

fn default_alignment_results() -> usize {
    25
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindAlignmentsArgs {
    #[schemars(range(min = 1))]
    pub panel_id: u64,
    #[schemars(range(min = 1))]
    pub track_id: u64,
    pub selector: ToolAlignmentSelector,
    #[serde(default = "default_alignment_results")]
    #[schemars(range(min = 1, max = 128))]
    pub maximum_results: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "selector", rename_all = "snake_case")]
pub enum ToolAlignmentSelector {
    ExactReadName {
        read_name: String,
    },
    Position {
        position: u64,
    },
    Interval {
        start: u64,
        end: u64,
    },
    Semantic {
        alignment_state: Option<ToolAlignmentState>,
        pair_class: Option<String>,
        has_mapped_mate: Option<bool>,
        has_supplementary_chain: Option<bool>,
        has_modified_bases: Option<bool>,
    },
}
impl ToolAlignmentSelector {
    fn into_core(self) -> AutomationAlignmentSelector {
        match self {
            Self::ExactReadName { read_name } => {
                AutomationAlignmentSelector::ExactReadName { read_name }
            }
            Self::Position { position } => AutomationAlignmentSelector::Position { position },
            Self::Interval { start, end } => AutomationAlignmentSelector::Interval { start, end },
            Self::Semantic {
                alignment_state,
                pair_class,
                has_mapped_mate,
                has_supplementary_chain,
                has_modified_bases,
            } => AutomationAlignmentSelector::Semantic {
                alignment_state: alignment_state.map(ToolAlignmentState::into_core),
                pair_class,
                has_mapped_mate,
                has_supplementary_chain,
                has_modified_bases,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolAlignmentState {
    Primary,
    Secondary,
    Supplementary,
}
impl ToolAlignmentState {
    fn into_core(self) -> AutomationAlignmentState {
        match self {
            Self::Primary => AutomationAlignmentState::Primary,
            Self::Secondary => AutomationAlignmentState::Secondary,
            Self::Supplementary => AutomationAlignmentState::Supplementary,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AlignmentHandleArgs {
    pub handle: String,
    pub expected_workspace_revision: Option<u64>,
    #[serde(default)]
    pub reveal: ToolAlignmentReveal,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolAlignmentReveal {
    #[default]
    None,
    Center,
    Fit,
}
impl ToolAlignmentReveal {
    fn into_core(self) -> AutomationAlignmentReveal {
        match self {
            Self::None => AutomationAlignmentReveal::None,
            Self::Center => AutomationAlignmentReveal::Center,
            Self::Fit => AutomationAlignmentReveal::Fit,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RelatedNavigationArgs {
    pub relationship: ToolAlignmentRelationship,
    pub segment_index: Option<usize>,
    #[serde(default)]
    pub destination: ToolNavigationDestination,
    pub expected_workspace_revision: Option<u64>,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolAlignmentRelationship {
    Mate,
    SupplementarySegment,
}
impl ToolAlignmentRelationship {
    fn into_core(self) -> AutomationAlignmentRelationship {
        match self {
            Self::Mate => AutomationAlignmentRelationship::Mate,
            Self::SupplementarySegment => AutomationAlignmentRelationship::SupplementarySegment,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolNavigationDestination {
    #[default]
    CurrentPanel,
    NewPanel,
}
impl ToolNavigationDestination {
    fn into_core(self) -> AutomationNavigationDestination {
        match self {
            Self::CurrentPanel => AutomationNavigationDestination::CurrentPanel,
            Self::NewPanel => AutomationNavigationDestination::NewPanel,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CaptureArgs {
    #[serde(default)]
    pub scope: ToolSnapshotScope,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CaptureWindowArgs {
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolSnapshotScope {
    #[default]
    VisibleWorkspace,
    ActivePanel,
    FullActivePanel,
}

impl ToolSnapshotScope {
    fn into_core(self) -> AutomationSnapshotScope {
        match self {
            Self::VisibleWorkspace => AutomationSnapshotScope::VisibleWorkspace,
            Self::ActivePanel => AutomationSnapshotScope::ActivePanel,
            Self::FullActivePanel => AutomationSnapshotScope::FullActivePanel,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExportArgs {
    pub path: String,
    pub kind: ToolExportKind,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolExportKind {
    Alignments,
    Pileup,
    SelectedEvidence,
    Modifications,
    Junctions,
    Variants,
    Quantitative,
    Regions,
    TrackNames,
}

impl ToolExportKind {
    fn into_core(self) -> AutomationExportKind {
        match self {
            Self::Alignments => AutomationExportKind::Alignments,
            Self::Pileup => AutomationExportKind::Pileup,
            Self::SelectedEvidence => AutomationExportKind::SelectedEvidence,
            Self::Modifications => AutomationExportKind::Modifications,
            Self::Junctions => AutomationExportKind::Junctions,
            Self::Variants => AutomationExportKind::Variants,
            Self::Quantitative => AutomationExportKind::Quantitative,
            Self::Regions => AutomationExportKind::Regions,
            Self::TrackNames => AutomationExportKind::TrackNames,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SaveArgs {
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenWorkspaceArgs {
    pub path: String,
    pub confirm_replace: bool,
    #[serde(default)]
    pub unsaved_changes: ToolUnsavedChangesPolicy,
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolUnsavedChangesPolicy {
    #[default]
    Reject,
    SaveThenOpen,
    Discard,
}
impl ToolUnsavedChangesPolicy {
    fn into_core(self) -> AutomationUnsavedChangesPolicy {
        match self {
            Self::Reject => AutomationUnsavedChangesPolicy::Reject,
            Self::SaveThenOpen => AutomationUnsavedChangesPolicy::SaveThenOpen,
            Self::Discard => AutomationUnsavedChangesPolicy::Discard,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecuteBatchArgs {
    #[schemars(length(min = 1, max = 100))]
    pub commands: Vec<Value>,
    #[serde(default)]
    pub allow_destructive: bool,
}

#[cfg(test)]
mod tests {
    use std::thread;

    use rmcp::{ServiceExt as _, model::CallToolRequestParams};
    use tiny_http::{Header, Response, Server};

    use super::*;

    #[test]
    fn public_report_redacts_path_url_and_source_fields_recursively() {
        let value = public_data(&json!({
            "track_id": 1,
            "path": "/private/data.bam",
            "nested": {"source_url": "https://example.test/?token=secret"},
            "read_name": "sensitive-record-id",
            "notes": "clinical note"
        }));
        assert_eq!(value["track_id"], 1);
        assert_eq!(value["path"], "[confined or redacted]");
        assert_eq!(value["nested"]["source_url"], "[confined or redacted]");
        assert_eq!(value["read_name"], "[confined or redacted]");
        assert_eq!(value["notes"], "[confined or redacted]");
        assert!(!value.to_string().contains("secret"));
        assert!(!value.to_string().contains("sensitive-record-id"));
    }

    #[test]
    fn raw_batch_restrictions_are_distinct_from_shared_validation() {
        let batch =
            AutomationBatch::from_json(r#"{"schema_version":1,"commands":[{"command":"exit"}]}"#)
                .unwrap();
        assert!(
            batch
                .commands
                .iter()
                .any(|command| matches!(command, AutomationCommand::Exit))
        );
        assert_eq!(MAXIMUM_MCP_BATCH_COMMANDS, 100);
    }

    #[test]
    fn png_dimensions_require_a_bounded_ihdr_prefix() {
        let mut header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        header.extend_from_slice(&640_u32.to_be_bytes());
        header.extend_from_slice(&480_u32.to_be_bytes());
        assert_eq!(png_dimensions(&header), Some((640, 480)));
        assert_eq!(png_dimensions(b"not a png"), None);
    }

    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn in_process_protocol_lists_contract_and_calls_observation_tools() {
        let control_server = Server::http("127.0.0.1:0").unwrap();
        let control_address = control_server.server_addr().to_ip().unwrap();
        let control_worker = thread::spawn(move || {
            for _ in 0..2 {
                let request = control_server.recv().unwrap();
                let body = if request.url() == "/v1/status" {
                    json!({
                        "schema_version": AUTOMATION_SCHEMA_VERSION,
                        "control_schema_version": AUTOMATION_SCHEMA_VERSION,
                        "application_version": "0.1.0",
                        "service": "rgv-local-control",
                        "authentication": "bearer",
                        "capabilities": ["workspace_state", "selection_state", "relative_viewport"]
                    })
                } else {
                    assert_eq!(request.url(), "/v1/batches");
                    assert!(request.headers().iter().any(|header| {
                        header.field.equiv("Authorization")
                            && header.value.as_str() == "Bearer protocol-test-secret-0123456789"
                    }));
                    json!({
                        "schema_version": AUTOMATION_SCHEMA_VERSION,
                        "succeeded": true,
                        "results": [{
                            "command_index": 0,
                            "command": "describe_workspace",
                            "status": "succeeded",
                            "message": "described workspace",
                            "data": {
                                "application_version": "0.1.0",
                                "schema_version": AUTOMATION_SCHEMA_VERSION,
                                "genome": {
                                    "name": "fixture",
                                    "reference_configured": false,
                                    "reference_ready": false
                                },
                                "settled": true,
                                "active_panel_id": 1,
                                "panels": [],
                                "tracks": [],
                                "status": [],
                                "truncated": false
                            }
                        }]
                    })
                };
                request
                    .respond(Response::from_string(body.to_string()).with_header(
                        Header::from_bytes("Content-Type", "application/json").unwrap(),
                    ))
                    .unwrap();
            }
        });

        let root = tempfile::tempdir().unwrap();
        let server = RgvMcpServer {
            control: ControlClient::new(
                reqwest::Url::parse(&format!("http://{control_address}")).unwrap(),
                "protocol-test-secret-0123456789".to_owned(),
            )
            .unwrap(),
            artifacts: ArtifactStore::new(root.path(), Path::new("mcp-artifacts")).unwrap(),
        };
        let (server_transport, client_transport) = tokio::io::duplex(256 * 1024);
        let server_task = tokio::spawn(async move {
            server
                .serve(server_transport)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        });
        let client = ().serve(client_transport).await.unwrap();

        let tools = client.list_all_tools().await.unwrap();
        let mut names = tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "rgv_capture_view",
                "rgv_capture_window",
                "rgv_change_viewport",
                "rgv_clear_selection",
                "rgv_configure",
                "rgv_execute_batch",
                "rgv_export",
                "rgv_filter_alignments",
                "rgv_find_alignments",
                "rgv_get_interaction_trace",
                "rgv_get_navigation_history",
                "rgv_get_related_alignments",
                "rgv_get_review_queue",
                "rgv_get_selection",
                "rgv_get_workspace",
                "rgv_group_alignments",
                "rgv_load_track",
                "rgv_navigate",
                "rgv_navigate_history",
                "rgv_navigate_related_alignment",
                "rgv_navigate_review_item",
                "rgv_open_workspace",
                "rgv_refresh_selection",
                "rgv_remove_track",
                "rgv_save_workspace",
                "rgv_select",
                "rgv_select_alignment",
                "rgv_sort_alignments",
                "rgv_status",
            ]
        );
        assert!(tools.iter().all(|tool| tool.annotations.is_some()));
        assert!(tools.iter().all(|tool| tool.description.is_some()));
        let configure_schema = serde_json::to_string(
            tools
                .iter()
                .find(|tool| tool.name == "rgv_configure")
                .unwrap()
                .input_schema
                .as_ref(),
        )
        .unwrap();
        assert!(configure_schema.contains("show_reference"));
        assert!(configure_schema.contains("window_size"));
        assert!(configure_schema.contains("interface_scale_percent"));
        assert!(configure_schema.contains("properties_panel_expanded"));
        assert!(configure_schema.contains("properties_panel_width"));
        assert!(configure_schema.contains("vertical_scroll_points"));
        assert!(configure_schema.contains("presentation_node_visible"));
        assert!(configure_schema.contains("presentation_group_expanded"));
        assert!(configure_schema.contains("presentation_composite_visible"));
        assert!(configure_schema.contains("read_panel_scroll_points"));
        assert!(configure_schema.contains("read_panel_density"));
        assert!(configure_schema.contains("alignment_color_mode"));
        assert!(configure_schema.contains("read_base_display_mode"));
        assert!(configure_schema.contains("^[A-Za-z][A-Za-z0-9]$"));
        assert!(!configure_schema.contains("\"scope\""));
        assert!(!configure_schema.contains("\"key\""));
        let viewport_schema = serde_json::to_string(
            tools
                .iter()
                .find(|tool| tool.name == "rgv_change_viewport")
                .unwrap()
                .input_schema
                .as_ref(),
        )
        .unwrap();
        assert!(viewport_schema.contains("pan_fraction"));
        assert!(viewport_schema.contains("100.0"));
        let navigate_schema = serde_json::to_string(
            tools
                .iter()
                .find(|tool| tool.name == "rgv_navigate")
                .unwrap()
                .input_schema
                .as_ref(),
        )
        .unwrap();
        assert!(navigate_schema.contains("\"maxItems\":16"));
        let select_alignment_schema = serde_json::to_string(
            tools
                .iter()
                .find(|tool| tool.name == "rgv_select_alignment")
                .unwrap()
                .input_schema
                .as_ref(),
        )
        .unwrap();
        assert!(select_alignment_schema.contains("reveal"));
        assert!(select_alignment_schema.contains("center"));
        assert!(select_alignment_schema.contains("fit"));
        assert!(select_alignment_schema.contains("expected_workspace_revision"));
        for open_world_tool in ["rgv_load_track", "rgv_execute_batch"] {
            assert_eq!(
                tools
                    .iter()
                    .find(|tool| tool.name == open_world_tool)
                    .and_then(|tool| tool.annotations.as_ref())
                    .and_then(|annotations| annotations.open_world_hint),
                Some(true)
            );
        }
        assert_eq!(
            tools
                .iter()
                .find(|tool| tool.name == "rgv_remove_track")
                .and_then(|tool| tool.annotations.as_ref())
                .and_then(|annotations| annotations.destructive_hint),
            Some(true)
        );

        let status = client
            .call_tool(CallToolRequestParams::new("rgv_status"))
            .await
            .unwrap();
        assert_eq!(status.is_error, Some(false));
        assert_eq!(
            status.structured_content.unwrap()["status"]["service"],
            "rgv-local-control"
        );
        let workspace = client
            .call_tool(CallToolRequestParams::new("rgv_get_workspace"))
            .await
            .unwrap();
        assert_eq!(workspace.is_error, Some(false));
        assert_eq!(
            workspace.structured_content.unwrap()["workspace"]["genome"]["name"],
            "fixture"
        );

        client.cancel().await.unwrap();
        server_task.await.unwrap();
        control_worker.join().unwrap();
    }
}
