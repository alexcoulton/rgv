use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use rgv_control_client::{ControlClient, ControlClientError};
use rgv_core::{
    AUTOMATION_SCHEMA_VERSION, AlignmentAuxComparison, AlignmentAuxFilterValue,
    AlignmentCigarEventPredicate, AlignmentColorMode, AlignmentFlagPredicate, AlignmentGroupMode,
    AlignmentOrderDirection, AlignmentPredicateExpression, AlignmentPredicateScope,
    AlignmentSortMode, AlignmentViewPredicate, AutomationAlignmentRelationship,
    AutomationAlignmentReveal, AutomationAlignmentSelector, AutomationAlignmentState,
    AutomationBatch, AutomationCommand, AutomationCommandStatus, AutomationExportKind,
    AutomationHistoryNavigation, AutomationInteractionTrace, AutomationNavigationDestination,
    AutomationReviewNavigation, AutomationRunReport, AutomationSelection,
    AutomationSelectionGranularity, AutomationSetting, AutomationSnapshotFormat,
    AutomationSnapshotScope, AutomationUnsavedChangesPolicy, AutomationViewportOperation,
    AutomationWorkspaceSnapshot, CoverageScaleTransform, ModifiedBaseColorMode,
    OverlappingMatePolicy, PanelId, PresentationNodeId, ReadBaseDisplayMode, TrackChannel, TrackId,
    TrackKind, TrackSource,
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
    artifacts::{ArtifactError, ArtifactStore, TemporaryArtifact},
    capture::{InlineCapture, effective_max_width, prepare_inline_png},
    config::RgvMcpArgs,
    redaction::{inline_export_read_names, redact_message},
};

/// Oldest viewer automation schema this adapter can drive. Schema 6 removed and reshaped
/// workspace-snapshot fields that earlier viewers emit, and a viewer rejects any batch stamped
/// above its own version, so nothing below 6 is supported.
pub(crate) const MINIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION: u32 = 6;
/// Newest viewer automation schema this adapter was built against. A newer viewer may have
/// changed response shapes this build cannot interpret.
pub(crate) const MAXIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION: u32 = AUTOMATION_SCHEMA_VERSION;
/// Version stamped on every batch. A viewer accepts batches stamped at or below its own schema
/// version, so the oldest supported version is accepted by every supported viewer.
const BATCH_SCHEMA_VERSION: u32 = MINIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION;
const _: () = assert!(
    MINIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION <= MAXIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION
        && rgv_core::MINIMUM_AUTOMATION_SCHEMA_VERSION <= BATCH_SCHEMA_VERSION
);

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
            viewer_schema_compatibility(status.schema_version, status.control_schema_version)
                .is_ok()
        })
    }

    async fn execute(&self, commands: Vec<AutomationCommand>) -> CallToolResult {
        let batch = AutomationBatch {
            schema_version: BATCH_SCHEMA_VERSION,
            commands,
        };
        match self.control.run_batch(&batch).await {
            Ok(report) if report.succeeded => success_result(
                "RGV completed the requested live-window operation",
                json!({"report": public_report(&report, ReportOptions::default())}),
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
            schema_version: BATCH_SCHEMA_VERSION,
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

    /// Runs `[wait_until_settled,] snapshot, describe_workspace` and returns the successful
    /// report with the index of the snapshot result. The workspace result follows it.
    async fn run_capture(
        &self,
        temporary: &TemporaryArtifact,
        scope: AutomationSnapshotScope,
        wait_until_settled: bool,
        timeout_millis: u64,
    ) -> Result<(AutomationRunReport, usize), CallToolResult> {
        let mut commands = Vec::new();
        if wait_until_settled {
            commands.push(AutomationCommand::WaitUntilSettled { timeout_millis });
        }
        let snapshot_result_index = commands.len();
        commands.push(AutomationCommand::Snapshot {
            path: temporary.path().display().to_string(),
            scope,
            format: AutomationSnapshotFormat::Png,
            scale: rgv_core::default_snapshot_scale(),
        });
        commands.push(AutomationCommand::DescribeWorkspace);
        match self
            .control
            .run_batch(&AutomationBatch {
                schema_version: BATCH_SCHEMA_VERSION,
                commands,
            })
            .await
        {
            Ok(report) if report.succeeded => Ok((report, snapshot_result_index)),
            Ok(report) => Err(report_failure(&report)),
            Err(error) => Err(control_failure(&error)),
        }
    }
}

/// Reads the temporary capture, checks it against the viewer's own metadata, and downscales it
/// for inlining. Returns the prepared image and the snapshot command's result data.
async fn inline_capture(
    temporary: &TemporaryArtifact,
    report: &AutomationRunReport,
    snapshot_result_index: usize,
    max_width: Option<u32>,
    label: &str,
) -> Result<(InlineCapture, Value), CallToolResult> {
    let bytes = temporary
        .read_capture()
        .map_err(|error| artifact_failure(&error))?;
    let Some((png_width, png_height)) = png_dimensions(&bytes) else {
        return Err(adapter_failure(
            "artifact_error",
            &format!("RGV returned a malformed PNG {label}"),
        ));
    };
    let metadata = report
        .results
        .get(snapshot_result_index)
        .map(|result| &result.data)
        .cloned()
        .unwrap_or(Value::Null);
    if metadata.get("width").and_then(Value::as_u64) != Some(u64::from(png_width))
        || metadata.get("height").and_then(Value::as_u64) != Some(u64::from(png_height))
    {
        return Err(adapter_failure(
            "invalid_response",
            &format!("RGV {label} metadata did not match the PNG dimensions"),
        ));
    }
    let max_width = effective_max_width(max_width);
    let prepared = tokio::task::spawn_blocking(move || {
        prepare_inline_png(bytes, png_width, png_height, max_width)
    })
    .await;
    match prepared {
        Ok(Ok(capture)) => Ok((capture, metadata)),
        Ok(Err(error)) => Err(adapter_failure("artifact_error", &error.to_string())),
        Err(_) => Err(adapter_failure(
            "artifact_error",
            &format!("the PNG {label} could not be prepared for inlining"),
        )),
    }
}

/// Image fields shared by both capture tools. `width`/`height` describe the inlined PNG;
/// `source_width`/`source_height` describe the viewer's capture before any downscale.
fn capture_structure(capture: &InlineCapture) -> Value {
    json!({
        "mime_type": "image/png",
        "bytes": capture.png.len(),
        "width": capture.width,
        "height": capture.height,
        "source_width": capture.source_width,
        "source_height": capture.source_height,
        "downscaled": capture.downscaled(),
    })
}

fn merge_object(target: &mut Value, addition: Value) {
    if let (Some(target), Value::Object(addition)) = (target.as_object_mut(), addition) {
        target.extend(addition);
    }
}

/// Checks the schema versions a viewer reports against the range this adapter supports.
fn viewer_schema_compatibility(
    schema_version: u32,
    control_schema_version: Option<u32>,
) -> Result<(), String> {
    let supported =
        MINIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION..=MAXIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION;
    let unsupported = [Some(schema_version), control_schema_version]
        .into_iter()
        .flatten()
        .find(|version| !supported.contains(version));
    match unsupported {
        None => Ok(()),
        Some(version) => Err(format!(
            "RGV reports automation schema {version}, but this rgv-mcp build supports schema \
             versions {MINIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION} through \
             {MAXIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION}; use rgv-desktop and rgv-mcp builds from \
             compatible releases"
        )),
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
            Ok(status) => {
                match viewer_schema_compatibility(
                    status.schema_version,
                    status.control_schema_version,
                ) {
                    Ok(()) => success_result(
                        "The live RGV control service is reachable",
                        json!({
                            "connected": true,
                            "compatible": true,
                            "supported_schema_versions": {
                                "minimum": MINIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION,
                                "maximum": MAXIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION,
                            },
                            "status": status,
                        }),
                    ),
                    Err(message) => adapter_failure("unsupported_version", &message),
                }
            }
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
    async fn get_workspace(&self, Parameters(args): Parameters<WorkspaceArgs>) -> CallToolResult {
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
                json!({"workspace": public_workspace(&workspace, args.include_telemetry)}),
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
                anchor_position: args.anchor_position,
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
                anchor_position: args.anchor_position,
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
        // The guard removes the viewer-written PNG on every exit path below.
        let temporary = self.artifacts.temporary("png");
        let (report, snapshot_result_index) = match self
            .run_capture(
                &temporary,
                args.scope.into_core(),
                args.wait_until_settled,
                args.timeout_millis,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(failure) => return failure,
        };
        let (capture, metadata) = match inline_capture(
            &temporary,
            &report,
            snapshot_result_index,
            args.max_width,
            "capture",
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(failure) => return failure,
        };
        let workspace = report.results.last().and_then(|result| {
            serde_json::from_value::<AutomationWorkspaceSnapshot>(result.data.clone()).ok()
        });
        let mut structured = capture_structure(&capture);
        merge_object(
            &mut structured,
            json!({
                "scope": args.scope,
                "truncated": metadata.get("truncated").and_then(Value::as_bool).unwrap_or(false),
                "genome": workspace.as_ref().map(|workspace| &workspace.genome),
                "application_instance_id": workspace.as_ref().map(|workspace| &workspace.application_instance_id),
                "workspace_revision": workspace.as_ref().map(|workspace| workspace.workspace_revision),
                "selection_id": workspace.as_ref().and_then(|workspace| workspace.selection.as_ref()).map(|selection| &selection.selection_id),
                "evidence_revision": workspace.as_ref().and_then(|workspace| workspace.selection.as_ref()).map(|selection| selection.evidence_revision),
                "panels": workspace.as_ref().map(|workspace| &workspace.panels),
                "loci": workspace.as_ref().map(|workspace| {
                    workspace.panels.iter().map(|panel| panel.locus.as_str()).collect::<Vec<_>>()
                }).unwrap_or_default(),
                "report": public_report(&report, ReportOptions {
                    include_telemetry: args.include_telemetry,
                    // The workspace is summarised once above, not inlined a second time.
                    omit_data_at: Some(snapshot_result_index + 1),
                }),
            }),
        );
        let mut result = CallToolResult::success(vec![
            ContentBlock::text("Captured the settled live RGV view"),
            ContentBlock::image(BASE64_STANDARD.encode(&capture.png), "image/png"),
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
        // The guard removes the viewer-written PNG on every exit path below, including the
        // protocol and process-identity rejections that follow the capture.
        let temporary = self.artifacts.temporary("png");
        let (report, snapshot_result_index) = match self
            .run_capture(
                &temporary,
                AutomationSnapshotScope::NativeWindow,
                args.wait_until_settled,
                args.timeout_millis,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(failure) => return failure,
        };
        let workspace = report.results.last().and_then(|result| {
            serde_json::from_value::<AutomationWorkspaceSnapshot>(result.data.clone()).ok()
        });
        let Some(workspace) = workspace else {
            return adapter_failure(
                "invalid_response",
                "RGV omitted the workspace associated with its window capture",
            );
        };
        let status = match self.control.status().await {
            Ok(status) => status,
            Err(error) => return control_failure(&error),
        };
        if let Err(message) =
            viewer_schema_compatibility(status.schema_version, status.control_schema_version)
        {
            return adapter_failure("unsupported_version", &message);
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
        let (capture, _) = match inline_capture(
            &temporary,
            &report,
            snapshot_result_index,
            args.max_width,
            "window capture",
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(failure) => return failure,
        };
        let loci = workspace
            .panels
            .iter()
            .map(|panel| panel.locus.as_str())
            .collect::<Vec<_>>();
        let mut structured = capture_structure(&capture);
        merge_object(
            &mut structured,
            json!({
                "capture_kind": "native_window_client_area",
                "platform": std::env::consts::OS,
                "process_id": status.process_id,
                "includes_native_frame": false,
                "foreground_activated": false,
                "application_instance_id": &workspace.application_instance_id,
                "workspace_revision": workspace.workspace_revision,
                "selection_id": workspace.selection.as_ref().map(|selection| &selection.selection_id),
                "evidence_revision": workspace.selection.as_ref().map(|selection| selection.evidence_revision),
                "panels": &workspace.panels,
                "loci": loci,
                "report": public_report(&report, ReportOptions {
                    include_telemetry: args.include_telemetry,
                    // The workspace is summarised once above, not inlined a second time.
                    omit_data_at: Some(snapshot_result_index + 1),
                }),
            }),
        );
        let mut result = CallToolResult::success(vec![
            ContentBlock::text("Captured the native RGV window client area without activating it"),
            ContentBlock::image(BASE64_STANDARD.encode(&capture.png), "image/png"),
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
                schema_version: BATCH_SCHEMA_VERSION,
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
        let inline = match self
            .artifacts
            .read_durable_text(&path, MAXIMUM_INLINE_EXPORT_BYTES)
        {
            Ok(value) => value.map(|text| inline_export_read_names(&text, args.include_read_names)),
            Err(error) => return artifact_failure(&error),
        };
        success_result(
            "Exported bounded evidence from the live RGV window",
            json!({
                "kind": args.kind,
                "path": relative,
                "bytes": metadata.len(),
                "inline_text": inline.as_ref().map(|inline| &inline.text),
                "inline_read_names": inline.as_ref().map(|inline| inline.handling),
                "inline_distinct_read_names_replaced": inline
                    .as_ref()
                    .map(|inline| inline.distinct_read_names),
                "report": public_report(&report, ReportOptions::default()),
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
                schema_version: BATCH_SCHEMA_VERSION,
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
            json!({"path": relative, "report": public_report(&report, ReportOptions::default())}),
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
            "schema_version": BATCH_SCHEMA_VERSION,
            "commands": args.commands,
        });
        let batch = match AutomationBatch::from_json(&value.to_string()) {
            Ok(batch) => batch,
            Err(error) => {
                return adapter_failure(
                    "invalid_batch",
                    &format!(
                        "commands do not form a valid RGV automation batch: {}",
                        redact_message(&error.to_string())
                    ),
                );
            }
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
                json!({"report": public_report(&report, ReportOptions {
                    include_telemetry: args.include_telemetry,
                    omit_data_at: None,
                })}),
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

/// Maps a control-client error to its stable code and a diagnostic. Viewer- and
/// validator-authored text is passed through after redaction; transport and decoding errors stay
/// generic because their text can echo the control URL or response bytes.
fn control_failure_parts(error: &ControlClientError) -> (String, String) {
    match error {
        ControlClientError::Transport(_) => (
            "unavailable".to_owned(),
            "The configured RGV window is unavailable; start it with local control enabled and retry".to_owned(),
        ),
        ControlClientError::Http { code, message, .. } => (
            code.clone(),
            with_detail(
                &format!("RGV local control rejected the request ({code})"),
                message,
            ),
        ),
        ControlClientError::InvalidBatch(reason) => (
            "invalid_batch".to_owned(),
            with_detail("The generated RGV batch was invalid", &reason.to_string()),
        ),
        ControlClientError::Json(_) | ControlClientError::InvalidResponse(_) => (
            "invalid_response".to_owned(),
            "RGV local control returned malformed JSON".to_owned(),
        ),
        ControlClientError::ResponseTooLarge => (
            "response_too_large".to_owned(),
            "RGV local control exceeded the adapter response limit".to_owned(),
        ),
        ControlClientError::UnexpectedContentType => (
            "invalid_response".to_owned(),
            "RGV local control returned an unexpected content type".to_owned(),
        ),
        ControlClientError::StateReobservationFailed => (
            "state_reobservation_failed".to_owned(),
            "RGV state could not be re-observed after an interrupted mutation".to_owned(),
        ),
    }
}

/// Appends a redacted diagnostic to a fixed summary, or returns the summary alone when the
/// diagnostic is empty.
fn with_detail(summary: &str, detail: &str) -> String {
    let detail = redact_message(detail.trim());
    if detail.is_empty() {
        summary.to_owned()
    } else {
        format!("{summary}: {detail}")
    }
}

fn control_failure(error: &ControlClientError) -> CallToolResult {
    let (code, message) = control_failure_parts(error);
    adapter_failure(&code, &message)
}

fn artifact_failure(error: &ArtifactError) -> CallToolResult {
    adapter_failure("artifact_error", &redact_message(&error.to_string()))
}

/// The stable error code and redacted viewer diagnostic of the first failed command.
fn report_failure_parts(report: &AutomationRunReport) -> (String, String) {
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
        |result| {
            with_detail(
                &format!("RGV command {} failed", result.command),
                &result.message,
            )
        },
    );
    (code, message)
}

fn report_failure(report: &AutomationRunReport) -> CallToolResult {
    let (code, message) = report_failure_parts(report);
    let mut result = CallToolResult::error(vec![ContentBlock::text(message.clone())]);
    result.structured_content = Some(json!({
        "error": {"code": code, "message": message},
        "report": public_report(report, ReportOptions::default()),
    }));
    result
}

#[derive(Clone, Copy, Debug, Default)]
struct ReportOptions {
    /// Include the run's frame, GPU, navigation, and environment telemetry.
    include_telemetry: bool,
    /// Result index whose `data` is replaced by a marker because the tool already reports the
    /// same state in its own top-level fields.
    omit_data_at: Option<usize>,
}

const OMITTED_DATA_MARKER: &str = "[reported once in this tool result's top-level fields]";

fn public_report(report: &AutomationRunReport, options: ReportOptions) -> Value {
    let mut public = json!({
        "schema_version": report.schema_version,
        "succeeded": report.succeeded,
        "results": report.results.iter().enumerate().map(|(index, result)| json!({
            "command_index": result.command_index,
            "command": result.command,
            "status": result.status,
            "error_code": result.error_code,
            // The viewer's own diagnostic, with filesystem locations and credentials removed.
            "message": redact_message(&result.message),
            "data": if options.omit_data_at == Some(index) {
                Value::String(OMITTED_DATA_MARKER.to_owned())
            } else {
                public_data(&result.data)
            },
            "elapsed_ms": result.elapsed_ms,
        })).collect::<Vec<_>>(),
    });
    if options.include_telemetry {
        merge_object(&mut public, json!({"telemetry": report.telemetry}));
    }
    public
}

/// Prefix of the workspace-snapshot fields that carry alignment-scene cache accounting and
/// diagnostic counters (`alignment_scene_diagnostics`, `alignment_scene_budget_bytes`, ...)
/// rather than workspace state.
const WORKSPACE_TELEMETRY_PREFIX: &str = "alignment_scene_";

/// The workspace as returned to an MCP client: telemetry is opt-in, and free-text track
/// failures and status lines have filesystem locations and credentials removed.
fn public_workspace(workspace: &AutomationWorkspaceSnapshot, include_telemetry: bool) -> Value {
    let mut value = serde_json::to_value(workspace).unwrap_or(Value::Null);
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    if !include_telemetry {
        object.retain(|field, _| !field.starts_with(WORKSPACE_TELEMETRY_PREFIX));
    }
    let redact_in_place = |text: &mut Value| {
        if let Value::String(message) = text {
            *message = redact_message(message);
        }
    };
    if let Some(Value::Array(status)) = object.get_mut("status") {
        status.iter_mut().for_each(redact_in_place);
    }
    if let Some(Value::Array(tracks)) = object.get_mut("tracks") {
        for track in tracks {
            if let Some(failure) = track.get_mut("failure") {
                redact_in_place(failure);
            }
        }
    }
    value
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

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WorkspaceArgs {
    /// When true, include alignment-scene cache accounting and diagnostic counters (every
    /// top-level `alignment_scene_*` field) in the workspace snapshot. Default false: they are
    /// large and irrelevant to genomic interpretation.
    #[serde(default)]
    pub include_telemetry: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NavigateArgs {
    /// One to 16 locus strings; each becomes one panel, in order, replacing the current panel
    /// arrangement. Coordinates are one-based and inclusive at both ends, e.g.
    /// `chr1:1,000,001-1,100,000` (thousands separators optional).
    #[schemars(length(min = 1, max = 16))]
    pub loci: Vec<String>,
    /// When true (the default), wait after the operation until every query it triggered has
    /// settled, so the returned report describes the final state. Set false only to observe
    /// intermediate state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChangeViewportArgs {
    /// Positive stable panel ID as reported by `rgv_get_workspace` (`panels[].id`); it is an
    /// identity, not a display index.
    #[schemars(range(min = 1))]
    pub panel_id: u64,
    /// The pan, zoom, or fit to apply. Numeric positions are zero-based; intervals are half-open
    /// `[start, end)`.
    pub operation: ToolViewportOperation,
    /// When true (the default), wait after the operation until every query it triggered has
    /// settled, so the returned report describes the final state. Set false only to observe
    /// intermediate state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ToolViewportOperation {
    /// Pan by a signed number of bases.
    PanBases {
        /// Signed pan distance in bases (base pairs). Positive moves the view toward higher
        /// coordinates, negative toward lower. Must not be zero; the view is clamped to the contig.
        delta: i64,
    },
    /// Pan by a signed fraction of the visible span.
    PanFraction {
        /// Signed pan distance as a fraction of the currently visible span (1.0 = one full view
        /// width). Positive moves toward higher coordinates. Must not be zero; -100 to 100.
        #[schemars(range(min = -100.0, max = 100.0))]
        fraction: f64,
    },
    /// Zoom in or out around an anchor.
    Zoom {
        /// Magnification factor, 0.01 to 100: above 1 zooms in (the visible span is divided by the
        /// factor), below 1 zooms out.
        #[schemars(range(min = 0.01, max = 100.0))]
        factor: f64,
        /// Zero-based genomic position on the panel's current contig that keeps its screen position
        /// while zooming. Omit to zoom around the viewport centre.
        anchor_position: Option<u64>,
    },
    /// Show exactly one zero-based, half-open interval `[start, end)` of the panel's current
    /// contig.
    FitInterval {
        /// Zero-based inclusive start of the interval, in bases, on the panel's current contig.
        start: u64,
        /// Zero-based exclusive end of the interval (half-open `[start, end)`); must be greater
        /// than `start`. Equals the one-based inclusive end. Clamped to the contig length.
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
    /// Where to read the track from: a local file under the control root or an HTTP/HTTPS URL.
    pub source: ToolSource,
    /// Track kind. Omit to let RGV infer it from the source.
    pub kind: Option<ToolTrackKind>,
    /// Optional display name for the track.
    pub name: Option<String>,
    /// When true (the default), wait after the operation until every query it triggered has
    /// settled, so the returned report describes the final state. Set false only to observe
    /// intermediate state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolSource {
    /// A file on the viewer's machine.
    Local {
        /// Local file path. A relative path is resolved against the viewer's `--control-root`;
        /// any path that resolves outside that root is rejected.
        path: String,
    },
    /// A remote file.
    Remote {
        /// HTTP or HTTPS URL of the remote file. It is never echoed back in tool results.
        url: String,
    },
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
    /// Positive stable track ID as reported by `rgv_get_workspace` (`tracks[].id`); it is an
    /// identity, not a display index.
    #[schemars(range(min = 1))]
    pub track_id: u64,
    /// When true (the default), wait after the operation until every query it triggered has
    /// settled, so the returned report describes the final state. Set false only to observe
    /// intermediate state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfigureArgs {
    /// The single typed setting to apply, selected by its `setting` discriminator. The report gives
    /// `requested`, `effective`, `changed`, and `clamped`.
    pub change: ToolSetting,
    /// When true (the default), wait after the operation until every query it triggered has
    /// settled, so the returned report describes the final state. Set false only to observe
    /// intermediate state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
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
    /// One track row in a presentation channel.
    Track {
        /// Presentation channel (pane) that owns the target: `annotation` or `reads`.
        channel: ToolTrackChannel,
        /// Positive stable track ID as reported by `rgv_get_workspace` (`tracks[].id`); it is an
        /// identity, not a display index.
        #[schemars(range(min = 1))]
        track_id: u64,
    },
    /// One presentation group in a channel.
    Group {
        /// Presentation channel (pane) that owns the target: `annotation` or `reads`.
        channel: ToolTrackChannel,
        /// Positive presentation-group ID as reported in `presentation_groups` of the workspace
        /// snapshot.
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

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum ToolAlignmentPredicateScope {
    #[default]
    Anywhere,
    /// Restrict the predicate to one reference base.
    Position {
        /// Contig (reference sequence) name exactly as the workspace reports it, e.g. `chr1`. An
        /// unknown name matches nothing.
        contig: String,
        /// Zero-based genomic position on `contig` (one less than the one-based coordinate shown in
        /// a locus string).
        position: u64,
    },
    /// Restrict the predicate to one zero-based, half-open reference interval `[start, end)`.
    Interval {
        /// Contig (reference sequence) name exactly as the workspace reports it, e.g. `chr1`. An
        /// unknown name matches nothing.
        contig: String,
        /// Zero-based inclusive start on `contig`, in bases.
        start: u64,
        /// Zero-based exclusive end on `contig` (half-open `[start, end)`); must be greater than
        /// `start`.
        end: u64,
    },
}

impl ToolAlignmentPredicateScope {
    fn into_core(self) -> AlignmentPredicateScope {
        match self {
            Self::Anywhere => AlignmentPredicateScope::Anywhere,
            Self::Position { contig, position } => {
                AlignmentPredicateScope::Position { contig, position }
            }
            Self::Interval { contig, start, end } => {
                AlignmentPredicateScope::Interval { contig, start, end }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolAlignmentFlagPredicate {
    Duplicate,
    Secondary,
    Supplementary,
    FailedQualityControl,
    Paired,
    ProperPair,
    MateMapped,
    ReverseStrand,
}

impl ToolAlignmentFlagPredicate {
    const fn into_core(self) -> AlignmentFlagPredicate {
        match self {
            Self::Duplicate => AlignmentFlagPredicate::Duplicate,
            Self::Secondary => AlignmentFlagPredicate::Secondary,
            Self::Supplementary => AlignmentFlagPredicate::Supplementary,
            Self::FailedQualityControl => AlignmentFlagPredicate::FailedQualityControl,
            Self::Paired => AlignmentFlagPredicate::Paired,
            Self::ProperPair => AlignmentFlagPredicate::ProperPair,
            Self::MateMapped => AlignmentFlagPredicate::MateMapped,
            Self::ReverseStrand => AlignmentFlagPredicate::ReverseStrand,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolAlignmentCigarEventPredicate {
    Insertion,
    Deletion,
    Indel,
    SoftClip,
    HardClip,
    ReferenceSkip,
}

impl ToolAlignmentCigarEventPredicate {
    const fn into_core(self) -> AlignmentCigarEventPredicate {
        match self {
            Self::Insertion => AlignmentCigarEventPredicate::Insertion,
            Self::Deletion => AlignmentCigarEventPredicate::Deletion,
            Self::Indel => AlignmentCigarEventPredicate::Indel,
            Self::SoftClip => AlignmentCigarEventPredicate::SoftClip,
            Self::HardClip => AlignmentCigarEventPredicate::HardClip,
            Self::ReferenceSkip => AlignmentCigarEventPredicate::ReferenceSkip,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolAlignmentAuxComparison {
    Exists,
    Equal,
    NotEqual,
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,
}

impl ToolAlignmentAuxComparison {
    const fn into_core(self) -> AlignmentAuxComparison {
        match self {
            Self::Exists => AlignmentAuxComparison::Exists,
            Self::Equal => AlignmentAuxComparison::Equal,
            Self::NotEqual => AlignmentAuxComparison::NotEqual,
            Self::Greater => AlignmentAuxComparison::Greater,
            Self::GreaterOrEqual => AlignmentAuxComparison::GreaterOrEqual,
            Self::Less => AlignmentAuxComparison::Less,
            Self::LessOrEqual => AlignmentAuxComparison::LessOrEqual,
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ToolAlignmentAuxValue {
    /// Integer operand, compared numerically with integer tag values.
    Integer(i64),
    /// Floating-point operand.
    Float(f64),
    /// String operand, compared with string (`Z`) tag values.
    String(String),
}

impl ToolAlignmentAuxValue {
    fn into_core(self) -> AlignmentAuxFilterValue {
        match self {
            Self::Integer(value) => AlignmentAuxFilterValue::Integer(value),
            Self::Float(value) => AlignmentAuxFilterValue::FloatBits(value.to_bits()),
            Self::String(value) => AlignmentAuxFilterValue::String(value),
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(tag = "predicate", rename_all = "snake_case")]
pub enum ToolAlignmentPredicate {
    /// Keep records whose mapping quality (MAPQ) lies in an inclusive range. Records without a
    /// mapping quality never match.
    MappingQuality {
        /// Inclusive lower bound on MAPQ (0 to 255). Omit for no lower bound.
        minimum: Option<u8>,
        /// Inclusive upper bound on MAPQ (0 to 255). Omit for no upper bound.
        maximum: Option<u8>,
    },
    /// Keep records by the state of one SAM flag.
    Flag {
        /// The SAM flag (or derived flag state) to test.
        flag: ToolAlignmentFlagPredicate,
        /// Required flag state: true keeps records with the flag set, false keeps records with it
        /// clear.
        expected: bool,
    },
    /// Keep records whose absolute template length (|TLEN|) lies in an inclusive range.
    TemplateLength {
        /// Inclusive lower bound on the absolute template length |TLEN|, in bases. Omit for no
        /// lower bound.
        minimum: Option<u64>,
        /// Inclusive upper bound on the absolute template length |TLEN|, in bases. Omit for no
        /// upper bound.
        maximum: Option<u64>,
    },
    /// Keep records whose mate maps to a named contig.
    MateContig {
        /// Contig name of the mate, exactly as the workspace reports it. An unknown name matches
        /// nothing.
        contig: String,
    },
    /// Keep records by the presence or value of one SAM auxiliary tag.
    Auxiliary {
        /// Two-character SAM auxiliary tag, e.g. `HP` or `NM` (a letter followed by a letter or
        /// digit; case-sensitive).
        #[schemars(length(min = 2, max = 2))]
        #[schemars(regex(pattern = r"^[A-Za-z][A-Za-z0-9]$"))]
        tag: String,
        /// How the tag is tested. `exists` needs no `value`; every other comparison is false for
        /// records lacking the tag.
        comparison: ToolAlignmentAuxComparison,
        /// Operand for the comparison. Required for every comparison except `exists`; without it
        /// those comparisons match nothing.
        value: Option<ToolAlignmentAuxValue>,
    },
    /// Keep records containing a CIGAR event of a given kind, optionally within a scope and length
    /// range.
    CigarEvent {
        /// The kind of CIGAR event to look for.
        event: ToolAlignmentCigarEventPredicate,
        /// Where the event must occur. Defaults to `anywhere` in the record.
        #[serde(default)]
        scope: ToolAlignmentPredicateScope,
        /// Inclusive lower bound on the event length, in bases. Omit for no lower bound.
        minimum_length: Option<u32>,
        /// Inclusive upper bound on the event length, in bases. Omit for no upper bound.
        maximum_length: Option<u32>,
    },
    /// Keep records whose aligned length lies in an inclusive range.
    AlignedLength {
        /// Inclusive lower bound on the aligned length: the sum of CIGAR `M`, `=`, and `X`
        /// operation lengths, in bases (not the reference span). Omit for no lower bound.
        minimum: Option<u64>,
        /// Inclusive upper bound on the aligned length: the sum of CIGAR `M`, `=`, and `X`
        /// operation lengths, in bases. Omit for no upper bound.
        maximum: Option<u64>,
    },
    /// Keep records by their base substitutions relative to the reference. Requires a loaded
    /// reference sequence.
    Substitution {
        /// Where substitutions are counted. Defaults to `anywhere` in the record.
        #[serde(default)]
        scope: ToolAlignmentPredicateScope,
        /// Single ASCII base letter the read must show at a substituted position, e.g. `T`. Omit to
        /// count any substituted base.
        observed_base: Option<char>,
        /// Inclusive lower bound on the number of matching substitutions within `scope`. Omit for
        /// no lower bound.
        minimum_count: Option<u32>,
        /// Inclusive upper bound on the number of matching substitutions within `scope`. Omit for
        /// no upper bound.
        maximum_count: Option<u32>,
    },
}

impl ToolAlignmentPredicate {
    fn into_core(self) -> AlignmentViewPredicate {
        match self {
            Self::MappingQuality { minimum, maximum } => {
                AlignmentViewPredicate::MappingQuality { minimum, maximum }
            }
            Self::Flag { flag, expected } => AlignmentViewPredicate::Flag {
                flag: flag.into_core(),
                expected,
            },
            Self::TemplateLength { minimum, maximum } => {
                AlignmentViewPredicate::TemplateLength { minimum, maximum }
            }
            Self::MateContig { contig } => AlignmentViewPredicate::MateContig { contig },
            Self::Auxiliary {
                tag,
                comparison,
                value,
            } => {
                let bytes = tag.as_bytes();
                AlignmentViewPredicate::Auxiliary {
                    tag: [
                        bytes.first().copied().unwrap_or_default(),
                        bytes.get(1).copied().unwrap_or_default(),
                    ],
                    comparison: comparison.into_core(),
                    value: value.map(ToolAlignmentAuxValue::into_core),
                }
            }
            Self::CigarEvent {
                event,
                scope,
                minimum_length,
                maximum_length,
            } => AlignmentViewPredicate::CigarEvent {
                event: event.into_core(),
                scope: scope.into_core(),
                minimum_length,
                maximum_length,
            },
            Self::AlignedLength { minimum, maximum } => {
                AlignmentViewPredicate::AlignedLength { minimum, maximum }
            }
            Self::Substitution {
                scope,
                observed_base,
                minimum_count,
                maximum_count,
            } => AlignmentViewPredicate::Substitution {
                scope: scope.into_core(),
                // Preserve an invalid non-ASCII request as an invalid byte so the canonical
                // automation validator rejects it; never reinterpret it as the omitted/"any"
                // base case.
                observed_base: observed_base.map(|base| u8::try_from(base).unwrap_or_default()),
                minimum_count,
                maximum_count,
            },
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "setting", rename_all = "snake_case")]
pub enum ToolSetting {
    WindowSize {
        /// Window client-area width in logical points (not physical pixels), 640 to 3840.
        #[schemars(range(min = 640, max = 3840))]
        width: u32,
        /// Window client-area height in logical points (not physical pixels), 400 to 2160.
        #[schemars(range(min = 400, max = 2160))]
        height: u32,
    },
    InterfaceScalePercent {
        /// Interface zoom in percent, 75 to 200; 100 is the unscaled interface.
        #[schemars(range(min = 75, max = 200))]
        value: u16,
    },
    PropertiesPanelExpanded {
        /// True expands the Properties panel, false collapses it.
        value: bool,
    },
    PropertiesPanelWidth {
        /// Properties panel width in logical points, 280 to 520.
        #[schemars(range(min = 280.0, max = 520.0))]
        value: f32,
    },
    PropertiesPanelResizeActive {
        /// True marks a held Properties resize sash (a transient qualification signal); set it back
        /// to false to release it and trigger the final refresh.
        value: bool,
    },
    ShowReference {
        /// True shows the reference sequence lane, false hides it.
        value: bool,
    },
    ShowCoverage {
        /// True shows alignment coverage, false hides it.
        value: bool,
    },
    ShowReads {
        /// True shows individual reads, false hides them.
        value: bool,
    },
    ShowJunctions {
        /// True shows splice-junction arcs, false hides them.
        value: bool,
    },
    ViewAsPairs {
        /// True draws paired reads as linked templates, false draws each read separately.
        value: bool,
    },
    OverlappingMatePolicy {
        /// How coverage counts bases where two mates of one template overlap.
        value: ToolOverlappingMatePolicy,
    },
    AlignmentColorMode {
        /// Positive stable alignment-track IDs (`tracks[].id` from `rgv_get_workspace`); 1 to 1024
        /// entries.
        #[schemars(length(min = 1, max = 1024))]
        track_ids: Vec<u64>,
        /// The read colouring mode to apply to the listed tracks.
        value: ToolAlignmentColorMode,
    },
    AlignmentColorTag {
        /// Positive stable alignment-track IDs (`tracks[].id` from `rgv_get_workspace`); 1 to 1024
        /// entries.
        #[schemars(length(min = 1, max = 1024))]
        track_ids: Vec<u64>,
        /// Two-character SAM auxiliary tag whose value colours each read, e.g. `HP` (a letter
        /// followed by a letter or digit; case-sensitive).
        #[schemars(length(min = 2, max = 2))]
        #[schemars(regex(pattern = r"^[A-Za-z][A-Za-z0-9]$"))]
        tag: String,
    },
    AlignmentViewFilter {
        /// Positive stable alignment-track IDs (`tracks[].id` from `rgv_get_workspace`); 1 to 1024
        /// entries.
        #[schemars(length(min = 1, max = 1024))]
        track_ids: Vec<u64>,
        /// Up to 64 predicates combined with logical AND; only records matching all of them remain
        /// in the view, coverage, and exports. An empty list clears the view filter.
        #[schemars(length(max = 64))]
        predicates: Vec<ToolAlignmentPredicate>,
    },
    AlignmentHighlightFilter {
        /// Positive stable alignment-track IDs (`tracks[].id` from `rgv_get_workspace`); 1 to 1024
        /// entries.
        #[schemars(length(min = 1, max = 1024))]
        track_ids: Vec<u64>,
        /// Up to 64 predicates combined with logical AND; matching records are highlighted but
        /// nothing is removed. An empty list clears the highlight.
        #[schemars(length(max = 64))]
        predicates: Vec<ToolAlignmentPredicate>,
    },
    AlignmentDensity {
        /// Read-row height multiplier, 0.25 to 4; 1.0 is the default row height and larger values
        /// draw taller rows.
        #[schemars(range(min = 0.25, max = 4.0))]
        value: f32,
    },
    IncludeSecondary {
        /// True includes secondary alignments (SAM flag 0x100), false excludes them.
        value: bool,
    },
    IncludeSupplementary {
        /// True includes supplementary alignments (SAM flag 0x800), false excludes them.
        value: bool,
    },
    CoverageScaleTransform {
        /// Vertical scale applied to coverage depth.
        value: ToolCoverageScaleTransform,
    },
    ReadBaseDisplayMode {
        /// Which read bases are drawn: mismatches only, all bases, or none.
        value: ToolReadBaseDisplayMode,
    },
    ShadeReadBasesByQuality {
        /// True shades read bases by their base quality, false draws them at full strength.
        value: bool,
    },
    ShowModifiedBases {
        /// True draws base-modification (MM/ML) calls, false hides them.
        value: bool,
    },
    ModifiedBaseColorMode {
        /// How base-modification calls are coloured.
        value: ToolModifiedBaseColorMode,
    },
    PresentationSectionExpanded {
        /// The presentation section (pane) to expand or collapse: `annotation` or `reads`.
        channel: ToolTrackChannel,
        /// True expands the section, false collapses it.
        value: bool,
    },
    PresentationNodeVisible {
        /// The reference lane, track, or group to show or hide.
        target: ToolPresentationNode,
        /// True shows the node, false hides it.
        value: bool,
    },
    PresentationNodeHeight {
        /// The reference lane, track, or group to resize.
        target: ToolPresentationNode,
        /// Preferred node height in logical points, 26 to 1200.
        #[schemars(range(min = 26.0, max = 1200.0))]
        value: f32,
    },
    PresentationGroupExpanded {
        /// The group to expand or collapse (a `group` node).
        target: ToolPresentationNode,
        /// True expands the group to show its members, false collapses it.
        value: bool,
    },
    PresentationGroupVisible {
        /// The group to show or hide (a `group` node).
        target: ToolPresentationNode,
        /// True shows the group, false hides it.
        value: bool,
    },
    PresentationCompositeVisible {
        /// Presentation channel (pane) that owns the composite: `annotation` or `reads`.
        channel: ToolTrackChannel,
        /// Positive composite ID as reported in `presentation_composites` of the workspace
        /// snapshot.
        #[schemars(range(min = 1))]
        composite_id: u32,
        /// True shows the composite, false hides it.
        value: bool,
    },
    PresentationGroup {
        /// Presentation channel (pane) in which to create the group: `annotation` or `reads`.
        channel: ToolTrackChannel,
        /// Positive stable track IDs (`tracks[].id`) to place in one new presentation group; 2 to
        /// 1024 entries, all in `channel`.
        #[schemars(length(min = 2, max = 1024))]
        track_ids: Vec<u64>,
    },
    PresentationUngroup {
        /// The group to dissolve (a `group` node).
        target: ToolPresentationNode,
    },
    ReadPaneHeight {
        /// Height of the reads pane in logical points, 100 to 1200.
        #[schemars(range(min = 100.0, max = 1200.0))]
        value: f32,
    },
    TrackHeight {
        /// Positive stable track ID as reported by `rgv_get_workspace` (`tracks[].id`); it is an
        /// identity, not a display index.
        #[schemars(range(min = 1))]
        track_id: u64,
        /// Track height in logical points, 40 to 1200.
        #[schemars(range(min = 40.0, max = 1200.0))]
        value: f32,
    },
    TrackCollapsed {
        /// Positive stable track ID as reported by `rgv_get_workspace` (`tracks[].id`); it is an
        /// identity, not a display index.
        #[schemars(range(min = 1))]
        track_id: u64,
        /// True collapses the track, false expands it.
        value: bool,
    },
    TrackVisible {
        /// Positive stable track ID as reported by `rgv_get_workspace` (`tracks[].id`); it is an
        /// identity, not a display index.
        #[schemars(range(min = 1))]
        track_id: u64,
        /// True shows the track, false hides it.
        value: bool,
    },
    VerticalScrollPoints {
        /// Positive stable panel ID as reported by `rgv_get_workspace` (`panels[].id`); it is an
        /// identity, not a display index.
        #[schemars(range(min = 1))]
        panel_id: u64,
        /// Vertical scroll offset of the panel in logical points from the top (0 = top). Clamped to
        /// the panel's `maximum_vertical_scroll_points`.
        #[schemars(range(min = 0.0))]
        value: f32,
    },
    AnnotationScrollPoints {
        /// Positive stable panel ID whose scrollbar issues the request. The annotation camera is
        /// workspace-wide, so the offset applies to every panel.
        #[schemars(range(min = 1))]
        panel_id: u64,
        /// Vertical scroll offset of the workspace-wide annotation pane in logical points from the
        /// top (0 = top). Clamped to `maximum_annotation_scroll_points`.
        #[schemars(range(min = 0.0))]
        value: f32,
    },
    ReadStackScrollPoints {
        /// Positive stable panel ID as reported by `rgv_get_workspace` (`panels[].id`); it is an
        /// identity, not a display index.
        #[schemars(range(min = 1))]
        panel_id: u64,
        /// Vertical scroll offset of this panel's read stack in logical points from the top (0 =
        /// top). Clamped to the panel's `maximum_read_stack_scroll_points`.
        #[schemars(range(min = 0.0))]
        value: f32,
    },
    ReadPanelScrollPoints {
        /// Positive stable panel ID as reported by `rgv_get_workspace` (`panels[].id`); it is an
        /// identity, not a display index.
        #[schemars(range(min = 1))]
        panel_id: u64,
        /// Positive stable track ID as reported by `rgv_get_workspace` (`tracks[].id`); it is an
        /// identity, not a display index.
        #[schemars(range(min = 1))]
        track_id: u64,
        /// Vertical scroll offset of this track's read panel in this genomic panel, in logical
        /// points from the top (0 = top). Clamped to the maximum reported in `read_panel_states`.
        #[schemars(range(min = 0.0))]
        value: f32,
    },
    ReadPanelDensity {
        /// Positive stable panel ID as reported by `rgv_get_workspace` (`panels[].id`); it is an
        /// identity, not a display index.
        #[schemars(range(min = 1))]
        panel_id: u64,
        /// Positive stable track ID as reported by `rgv_get_workspace` (`tracks[].id`); it is an
        /// identity, not a display index.
        #[schemars(range(min = 1))]
        track_id: u64,
        /// Read-row height multiplier for this track in this panel, 0.25 to 4; 1.0 is the default
        /// row height and larger values draw taller rows.
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
            Self::PropertiesPanelResizeActive { value } => {
                AutomationSetting::PropertiesPanelResizeActive { value }
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
            Self::AlignmentViewFilter {
                track_ids: ids,
                predicates,
            } => AutomationSetting::AlignmentViewFilter {
                track_ids: track_ids(&ids),
                value: AlignmentPredicateExpression {
                    version: 1,
                    all: predicates
                        .into_iter()
                        .map(ToolAlignmentPredicate::into_core)
                        .collect(),
                },
            },
            Self::AlignmentHighlightFilter {
                track_ids: ids,
                predicates,
            } => AutomationSetting::AlignmentHighlightFilter {
                track_ids: track_ids(&ids),
                value: AlignmentPredicateExpression {
                    version: 1,
                    all: predicates
                        .into_iter()
                        .map(ToolAlignmentPredicate::into_core)
                        .collect(),
                },
            },
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
            Self::PresentationSectionExpanded { channel, value } => {
                AutomationSetting::PresentationSectionExpanded {
                    channel: channel.into_core(),
                    value,
                }
            }
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
    /// Positive stable alignment-track IDs (`tracks[].id` from `rgv_get_workspace`); 1 to 1024
    /// entries.
    #[schemars(length(min = 1, max = 1024))]
    pub track_ids: Vec<u64>,
    /// Grouping mode. `tag` requires `tag`; `base` and `insertion` group by the evidence at
    /// `anchor_position`; `none` removes grouping.
    pub mode: ToolGroupMode,
    /// Order of the resulting groups: `natural` (default) or `reverse`.
    #[serde(default)]
    pub direction: ToolDirection,
    /// Two-character SAM auxiliary tag to group by, e.g. `HP` (a letter followed by a letter or
    /// digit). Required when `mode` is `tag`; ignored for every other mode.
    #[schemars(regex(pattern = r"^[A-Za-z][A-Za-z0-9]$"))]
    pub tag: Option<String>,
    /// Zero-based genomic position on the active panel's contig used by `base` and `insertion`
    /// grouping. Only valid with those modes; defaults to the centre of the active panel's
    /// viewport.
    pub anchor_position: Option<u64>,
    /// When true (the default), wait after the operation until every query it triggered has
    /// settled, so the returned report describes the final state. Set false only to observe
    /// intermediate state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
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
    Base,
    Insertion,
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
            Self::Base => AlignmentGroupMode::Base,
            Self::Insertion => AlignmentGroupMode::Insertion,
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
    /// Positive stable alignment-track IDs (`tracks[].id` from `rgv_get_workspace`); 1 to 1024
    /// entries.
    #[schemars(length(min = 1, max = 1024))]
    pub track_ids: Vec<u64>,
    /// Sort key. `tag` requires `tag`; `base` sorts by the read base at `anchor_position`.
    pub mode: ToolSortMode,
    /// Sort direction: `natural` (default) or `reverse`.
    #[serde(default)]
    pub direction: ToolDirection,
    /// Two-character SAM auxiliary tag to sort by, e.g. `HP` (a letter followed by a letter or
    /// digit). Required when `mode` is `tag`; ignored for every other mode.
    #[schemars(regex(pattern = r"^[A-Za-z][A-Za-z0-9]$"))]
    pub tag: Option<String>,
    /// Zero-based genomic position on the active panel's contig used by `base` sorting. Only valid
    /// with that mode; defaults to the centre of the active panel's viewport.
    pub anchor_position: Option<u64>,
    /// When true (the default), wait after the operation until every query it triggered has
    /// settled, so the returned report describes the final state. Set false only to observe
    /// intermediate state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
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
    /// Positive stable alignment-track IDs (`tracks[].id` from `rgv_get_workspace`); 1 to 1024
    /// entries.
    #[schemars(length(min = 1, max = 1024))]
    pub track_ids: Vec<u64>,
    /// RGV alignment-filter expression, validated by the viewer. An empty string clears the
    /// alignment filter.
    pub expression: String,
    /// When true (the default), wait after the operation until every query it triggered has
    /// settled, so the returned report describes the final state. Set false only to observe
    /// intermediate state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SelectArgs {
    /// What to select: a track, a panel, or one genomic position on a track in a panel.
    pub selection: ToolSelection,
    /// When true (the default), wait after the operation until every query it triggered has
    /// settled, so the returned report describes the final state. Set false only to observe
    /// intermediate state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "selection", rename_all = "snake_case")]
pub enum ToolSelection {
    /// Select a track.
    Track {
        /// Positive stable track ID as reported by `rgv_get_workspace` (`tracks[].id`); it is an
        /// identity, not a display index.
        #[schemars(range(min = 1))]
        track_id: u64,
    },
    /// Select a panel.
    Panel {
        /// Positive stable panel ID as reported by `rgv_get_workspace` (`panels[].id`); it is an
        /// identity, not a display index.
        #[schemars(range(min = 1))]
        panel_id: u64,
    },
    /// Select the evidence at one genomic position of a track in a panel.
    Position {
        /// Positive stable panel ID as reported by `rgv_get_workspace` (`panels[].id`); it is an
        /// identity, not a display index.
        #[schemars(range(min = 1))]
        panel_id: u64,
        /// Positive stable track ID as reported by `rgv_get_workspace` (`tracks[].id`); it is an
        /// identity, not a display index.
        #[schemars(range(min = 1))]
        track_id: u64,
        /// Zero-based genomic position on the panel's current contig (one less than the one-based
        /// coordinate shown in a locus string).
        position: u64,
        /// `exact_base` (default) selects evidence for exactly that base, which may require a
        /// one-base query for alignment evidence; `visible_bin` selects the currently rendered
        /// bin containing it.
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
    /// Positive stable panel ID as reported by `rgv_get_workspace` (`panels[].id`); it is an
    /// identity, not a display index.
    #[schemars(range(min = 1))]
    pub panel_id: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReviewNavigationArgs {
    /// Which review item to open: a specific item or the next pending one.
    pub action: ToolReviewNavigation,
    /// When true (the default), wait after the operation until every query it triggered has
    /// settled, so the returned report describes the final state. Set false only to observe
    /// intermediate state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ToolReviewNavigation {
    /// Navigate to one review item.
    Item {
        /// Positive stable review-item ID as reported by `rgv_get_review_queue`.
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
    /// Positive stable panel ID as reported by `rgv_get_workspace` (`panels[].id`); it is an
    /// identity, not a display index.
    #[schemars(range(min = 1))]
    pub panel_id: u64,
    /// Move one step back or forward, or jump to an entry by index.
    pub action: ToolHistoryNavigation,
    /// When true (the default), wait after the operation until every query it triggered has
    /// settled, so the returned report describes the final state. Set false only to observe
    /// intermediate state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ToolHistoryNavigation {
    Back,
    Forward,
    /// Jump to one history entry.
    Index {
        /// Zero-based index of the history entry, as reported in `index` by
        /// `rgv_get_navigation_history`.
        index: usize,
    },
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
    /// Positive stable panel ID as reported by `rgv_get_workspace` (`panels[].id`); it is an
    /// identity, not a display index.
    #[schemars(range(min = 1))]
    pub panel_id: u64,
    /// Positive stable track ID as reported by `rgv_get_workspace` (`tracks[].id`); it is an
    /// identity, not a display index.
    #[schemars(range(min = 1))]
    pub track_id: u64,
    /// Which records to find within the track's currently settled query scope.
    pub selector: ToolAlignmentSelector,
    /// Maximum number of hits to return, 1 to 128 (default 25).
    #[serde(default = "default_alignment_results")]
    #[schemars(range(min = 1, max = 128))]
    pub maximum_results: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "selector", rename_all = "snake_case")]
pub enum ToolAlignmentSelector {
    /// Find records by exact read name.
    ExactReadName {
        /// Exact, case-sensitive read name (QNAME). Treated as sensitive: matched only inside the
        /// desktop process and never echoed in results.
        read_name: String,
    },
    /// Find records covering one reference base.
    Position {
        /// Zero-based genomic position on the panel's current contig; matches records whose aligned
        /// span `[start, end)` contains it.
        position: u64,
    },
    /// Find records overlapping a zero-based, half-open interval `[start, end)`.
    Interval {
        /// Zero-based inclusive start on the panel's current contig, in bases.
        start: u64,
        /// Zero-based exclusive end (half-open `[start, end)`); must be greater than `start`.
        /// Records overlapping the interval by at least one base match.
        end: u64,
    },
    /// Find records by alignment properties; every supplied property must match.
    Semantic {
        /// Required alignment state: primary, secondary, or supplementary. Omit to accept any.
        alignment_state: Option<ToolAlignmentState>,
        /// Required pair classification, case-insensitive, with no separators: `unpaired`,
        /// `readunmapped`, `mateunmapped`, `matemissing`, `intercontig`, `discordantorientation`,
        /// `discordanttemplatelengthshort`, `discordanttemplatelengthlong`, `proper`, or
        /// `concordant` (the `pair_class` values returned in hits). Omit to accept any.
        pair_class: Option<String>,
        /// True requires a mapped mate with a known position, false requires none. Omit to accept
        /// either.
        has_mapped_mate: Option<bool>,
        /// True requires a navigable supplementary (split-read) chain, false requires none. Omit to
        /// accept either.
        has_supplementary_chain: Option<bool>,
        /// True requires base-modification calls, false requires none. Fails with
        /// `effective_state_unavailable` if modification evidence is not loaded. Omit to accept
        /// either.
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
    /// Opaque alignment handle returned by `rgv_find_alignments`. It expires when the backing query
    /// changes (`stale_handle`).
    pub handle: String,
    /// If set, the call fails with `workspace_revision_conflict` and changes nothing unless the
    /// viewer's current `workspace_revision` equals this value.
    pub expected_workspace_revision: Option<u64>,
    /// Viewport change applied with the selection: `none` (default) leaves the view, `center` keeps
    /// the span and centres the record, `fit` fits the record.
    #[serde(default)]
    pub reveal: ToolAlignmentReveal,
    /// When true (the default), wait after the operation until every query it triggered has
    /// settled, so the returned report describes the final state. Set false only to observe
    /// intermediate state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
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
    /// Which related record of the selected alignment to open: its mate or a supplementary-chain
    /// segment.
    pub relationship: ToolAlignmentRelationship,
    /// Zero-based index of the supplementary-chain segment, as reported in `segment_index` by
    /// `rgv_get_related_alignments`. Required for `supplementary_segment`; must be omitted for
    /// `mate`.
    pub segment_index: Option<usize>,
    /// Where to show the related record: the `current_panel` (default) or a `new_panel`.
    #[serde(default)]
    pub destination: ToolNavigationDestination,
    /// If set, the call fails with `workspace_revision_conflict` and changes nothing unless the
    /// viewer's current `workspace_revision` equals this value.
    pub expected_workspace_revision: Option<u64>,
    /// When true (the default), wait after the operation until every query it triggered has
    /// settled, so the returned report describes the final state. Set false only to observe
    /// intermediate state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
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
    /// What to capture: the visible workspace (default), the active panel's visible canvas, or the
    /// active panel's complete unscrolled content.
    #[serde(default)]
    pub scope: ToolSnapshotScope,
    /// Maximum width in pixels of the inlined PNG, 64 to 16384 (default 1600). A wider capture is
    /// downscaled with its aspect ratio preserved; a narrower one is never enlarged. The result
    /// reports both the inlined `width`/`height` and the original `source_width`/`source_height`.
    #[serde(default)]
    #[schemars(range(min = 64, max = 16_384))]
    pub max_width: Option<u32>,
    /// When true, include the run's frame, GPU, navigation, and environment performance telemetry
    /// in the report. Default false: telemetry is large and irrelevant to genomic interpretation.
    #[serde(default)]
    pub include_telemetry: bool,
    /// When true (the default), wait until all pending queries have settled before capturing, so it
    /// reflects the final rendered state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CaptureWindowArgs {
    /// Maximum width in pixels of the inlined PNG, 64 to 16384 (default 1600). A wider capture is
    /// downscaled with its aspect ratio preserved; a narrower one is never enlarged. The result
    /// reports both the inlined `width`/`height` and the original `source_width`/`source_height`.
    #[serde(default)]
    #[schemars(range(min = 64, max = 16_384))]
    pub max_width: Option<u32>,
    /// When true, include the run's frame, GPU, navigation, and environment performance telemetry
    /// in the report. Default false: telemetry is large and irrelevant to genomic interpretation.
    #[serde(default)]
    pub include_telemetry: bool,
    /// When true (the default), wait until all pending queries have settled before capturing, so it
    /// reflects the final rendered state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
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
    /// Destination file path relative to the control root, e.g. `exports/reads.tsv`. Absolute
    /// paths, `..` components, and symbolic links are rejected.
    pub path: String,
    /// Which evidence to export for the active panel's current interval. RGV's tabular exports
    /// state their convention in a `# coordinates` header line (one-based `start`, one-based
    /// inclusive `end`).
    pub kind: ToolExportKind,
    /// When false (the default), read names in the inlined `inline_text` are replaced by
    /// export-local pseudonyms (`read_1`, `read_2`, ...; segments of one template share a
    /// pseudonym) and selected-evidence read identity is redacted. Set true to inline the original
    /// read names. The file written under the control root always contains the original names.
    #[serde(default)]
    pub include_read_names: bool,
    /// When true (the default), wait until all pending queries have settled before exporting, so it
    /// reflects the final rendered state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Settlement timeout in milliseconds, 100 to 600000 (default 30000). Only used when
    /// `wait_until_settled` is true; a timeout does not roll back the operation.
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 100, max = 600_000))]
    pub timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolExportKind {
    Alignments,
    FilteredAlignments,
    EligibleAlignments,
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
            Self::FilteredAlignments => AutomationExportKind::FilteredAlignments,
            Self::EligibleAlignments => AutomationExportKind::EligibleAlignments,
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
    /// Destination project file path relative to the control root, e.g. `sessions/review.rgv.json`.
    /// Absolute paths, `..` components, and symbolic links are rejected.
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenWorkspaceArgs {
    /// Project file path relative to the control root. Must be an existing regular file; absolute
    /// paths, `..` components, and symbolic links are rejected.
    pub path: String,
    /// Must be true: opening a project replaces the entire visible workspace.
    pub confirm_replace: bool,
    /// What to do if the current workspace has unsaved changes: `reject` (default) fails with
    /// `unsaved_changes`, `save_then_open` saves first, `discard` drops them.
    #[serde(default)]
    pub unsaved_changes: ToolUnsavedChangesPolicy,
    /// When true (the default), wait after the operation until every query it triggered has
    /// settled, so the returned report describes the final state. Set false only to observe
    /// intermediate state.
    #[serde(default = "default_true")]
    pub wait_until_settled: bool,
    /// Timeout in milliseconds, 100 to 600000 (default 30000), for opening the project and, when
    /// `wait_until_settled` is true, for the settlement wait that follows.
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
    /// One to 100 RGV automation command objects, executed in order; each has a `command`
    /// discriminator (e.g. `{"command": "goto", "loci": ["chr1:101-200"]}`). Locus strings are
    /// one-based inclusive; numeric `position`/`start`/`end` fields are zero-based, half-open.
    /// `exit` is always rejected.
    #[schemars(length(min = 1, max = 100))]
    pub commands: Vec<Value>,
    /// Must be true for a batch containing `new_workspace`, which discards the visible workspace.
    /// Default false.
    #[serde(default)]
    pub allow_destructive: bool,
    /// When true, include the run's frame, GPU, navigation, and environment performance telemetry
    /// in the report. Default false: telemetry is large and irrelevant to genomic interpretation.
    #[serde(default)]
    pub include_telemetry: bool,
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

    fn failed_report(code: &str, message: &str) -> AutomationRunReport {
        serde_json::from_value(json!({
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "succeeded": false,
            "results": [
                {"command_index": 0, "command": "load_track", "status": "failed",
                 "error_code": code, "message": message},
                {"command_index": 1, "command": "wait_until_settled", "status": "skipped",
                 "message": "Skipped after an earlier failure"}
            ],
            "telemetry": {"run_elapsed_ms": 12.5, "navigation": [{"panel_id": 1}]}
        }))
        .unwrap()
    }

    #[test]
    fn viewer_failure_diagnostics_pass_through_with_paths_redacted() {
        let report = failed_report(
            "resource_not_found",
            "track source /Users/alice/cohort/NA12878.bam does not exist or is not a file",
        );
        let (code, message) = report_failure_parts(&report);
        assert_eq!(code, "resource_not_found");
        assert_eq!(
            message,
            "RGV command load_track failed: track source [path]/NA12878.bam does not exist or is not a file"
        );
        let failure = report_failure(&report);
        assert_eq!(failure.is_error, Some(true));
        let structured = failure.structured_content.unwrap();
        assert_eq!(structured["error"]["code"], "resource_not_found");
        assert_eq!(structured["error"]["message"], message.as_str());
        assert_eq!(
            structured["report"]["results"][0]["message"],
            "track source [path]/NA12878.bam does not exist or is not a file"
        );
        assert_eq!(
            structured["report"]["results"][1]["message"],
            "Skipped after an earlier failure"
        );
        assert!(!structured.to_string().contains("alice"));
        assert!(!structured.to_string().contains("Command failed"));
    }

    #[test]
    fn viewer_failure_without_a_code_or_message_keeps_a_stable_fallback() {
        let mut report = failed_report("timeout", "");
        report.results[0].error_code = None;
        assert_eq!(
            report_failure_parts(&report),
            (
                "viewer_failed".to_owned(),
                "RGV command load_track failed".to_owned()
            )
        );
        report.results.clear();
        assert_eq!(
            report_failure_parts(&report),
            (
                "viewer_failed".to_owned(),
                "RGV did not complete the requested operation".to_owned()
            )
        );
    }

    #[test]
    fn control_rejections_pass_the_viewer_message_through_without_secrets() {
        let (code, message) = control_failure_parts(&ControlClientError::Http {
            status: 400,
            code: "invalid_batch".to_owned(),
            message: "automation path /srv/other/reads.bam is outside the selected local-control root; Authorization: Bearer 0123456789abcdef0123456789abcdef".to_owned(),
        });
        assert_eq!(code, "invalid_batch");
        assert!(message.starts_with("RGV local control rejected the request (invalid_batch): "));
        assert!(message.contains("[path]/reads.bam is outside the selected local-control root"));
        assert!(!message.contains("/srv/other"));
        assert!(!message.contains("0123456789abcdef"));

        let invalid = AutomationBatch {
            schema_version: BATCH_SCHEMA_VERSION,
            commands: vec![AutomationCommand::WaitUntilSettled { timeout_millis: 1 }],
        };
        let (code, message) = control_failure_parts(&ControlClientError::InvalidBatch(
            invalid.validate().unwrap_err(),
        ));
        assert_eq!(code, "invalid_batch");
        assert!(message.contains("timeout_millis must be between 100 and 600000"));

        let (code, message) = control_failure_parts(&ControlClientError::ResponseTooLarge);
        assert_eq!(code, "response_too_large");
        assert!(!message.is_empty());
    }

    #[test]
    fn report_telemetry_is_opt_in_and_named_data_can_be_omitted() {
        let report = failed_report("timeout", "settlement timed out");
        let default = public_report(&report, ReportOptions::default());
        assert!(default.get("telemetry").is_none());
        assert_eq!(default["results"][0]["message"], "settlement timed out");
        let with_telemetry = public_report(
            &report,
            ReportOptions {
                include_telemetry: true,
                omit_data_at: Some(1),
            },
        );
        assert_eq!(with_telemetry["telemetry"]["run_elapsed_ms"], 12.5);
        assert_eq!(with_telemetry["results"][1]["data"], OMITTED_DATA_MARKER);
        assert_ne!(with_telemetry["results"][0]["data"], OMITTED_DATA_MARKER);
    }

    #[test]
    fn viewer_schema_versions_outside_the_supported_range_are_rejected_clearly() {
        let minimum = MINIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION;
        let maximum = MAXIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION;
        for version in minimum..=maximum {
            assert_eq!(viewer_schema_compatibility(version, None), Ok(()));
            assert_eq!(viewer_schema_compatibility(version, Some(version)), Ok(()));
        }
        let too_old = viewer_schema_compatibility(minimum - 1, Some(minimum - 1)).unwrap_err();
        assert!(too_old.contains(&format!("automation schema {}", minimum - 1)));
        assert!(too_old.contains(&format!("{minimum} through {maximum}")));
        let too_new = viewer_schema_compatibility(maximum + 1, None).unwrap_err();
        assert!(too_new.contains(&format!("automation schema {}", maximum + 1)));
        // The control schema is checked independently of the automation schema.
        let control = viewer_schema_compatibility(maximum, Some(maximum + 1)).unwrap_err();
        assert!(control.contains(&format!("automation schema {}", maximum + 1)));
        assert!(viewer_schema_compatibility(0, None).is_err());
        // Every supported viewer accepts the version stamped on generated batches.
        assert!(
            (rgv_core::MINIMUM_AUTOMATION_SCHEMA_VERSION..=minimum).contains(&BATCH_SCHEMA_VERSION)
        );
    }

    fn workspace_fixture() -> Value {
        json!({
            "application_version": "0.1.0",
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "application_instance_id": "instance-a",
            "workspace_revision": 7,
            "genome": {
                "name": "fixture",
                "reference_configured": false,
                "reference_ready": false
            },
            "settled": true,
            "active_panel_id": 1,
            "panels": [{
                "id": 1, "locus": "chr1:101-200", "contig": "chr1", "start": 100, "end": 200,
                "detail_level": "Bases", "vertical_scroll_points": 0.0
            }],
            "tracks": [{
                "id": 3, "name": "tumour", "kind": "Alignment", "visible": true,
                "collapsed": false, "height": 120.0, "lifecycle": "failed",
                "failure": "could not open /Users/alice/tumour.bam.bai"
            }],
            "status": ["Could not start export: /Users/alice/out.tsv is read-only"],
            "alignment_scene_budget_bytes": 4096,
            "alignment_scene_diagnostics": {"camera_updates": 9, "scene_publications": 2},
            "truncated": false
        })
    }

    #[test]
    fn workspace_telemetry_is_opt_in_and_free_text_is_redacted() {
        let workspace: AutomationWorkspaceSnapshot =
            serde_json::from_value(workspace_fixture()).unwrap();
        let default = public_workspace(&workspace, false);
        let telemetry_fields = |value: &Value| {
            value
                .as_object()
                .unwrap()
                .keys()
                .filter(|field| field.starts_with(WORKSPACE_TELEMETRY_PREFIX))
                .count()
        };
        assert_eq!(telemetry_fields(&default), 0);
        assert_eq!(default["panels"][0]["locus"], "chr1:101-200");
        assert_eq!(default["workspace_revision"], 7);
        assert_eq!(
            default["tracks"][0]["failure"],
            "could not open [path]/tumour.bam.bai"
        );
        assert_eq!(
            default["status"][0],
            "Could not start export: [path]/out.tsv is read-only"
        );
        let with_telemetry = public_workspace(&workspace, true);
        assert!(telemetry_fields(&with_telemetry) >= 2);
        assert_eq!(with_telemetry["alignment_scene_budget_bytes"], 4096);
        assert_eq!(
            with_telemetry["alignment_scene_diagnostics"]["camera_updates"],
            9
        );
        assert!(!with_telemetry.to_string().contains("alice"));
    }

    /// A scripted stand-in for the viewer's control service. `respond` receives the request
    /// path and parsed JSON body and returns the HTTP status and JSON response.
    fn fake_viewer(
        requests: usize,
        mut respond: impl FnMut(&str, &Value) -> (u16, Value) + Send + 'static,
    ) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let worker = thread::spawn(move || {
            for _ in 0..requests {
                let mut request = server.recv().unwrap();
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                let body = serde_json::from_str(&body).unwrap_or(Value::Null);
                let (status, response) = respond(request.url(), &body);
                request
                    .respond(
                        Response::from_string(response.to_string())
                            .with_status_code(status)
                            .with_header(
                                Header::from_bytes("Content-Type", "application/json").unwrap(),
                            ),
                    )
                    .unwrap();
            }
        });
        (address, worker)
    }

    fn test_server(address: std::net::SocketAddr, root: &Path) -> RgvMcpServer {
        RgvMcpServer {
            control: ControlClient::new(
                reqwest::Url::parse(&format!("http://{address}")).unwrap(),
                "protocol-test-secret-0123456789".to_owned(),
            )
            .unwrap(),
            artifacts: ArtifactStore::new(root, Path::new("mcp-artifacts")).unwrap(),
        }
    }

    fn temporary_artifact_count(root: &Path) -> usize {
        std::fs::read_dir(root.join("mcp-artifacts"))
            .unwrap()
            .count()
    }

    fn test_png(width: u32, height: u32) -> Vec<u8> {
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(width, height, image::Rgba([20, 40, 60, 255]))
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        encoded.into_inner()
    }

    /// Answers a capture batch the way the viewer does: writes the PNG to the requested path
    /// and reports its dimensions followed by the workspace.
    fn capture_response(body: &Value, reported_width: u32) -> (u16, Value) {
        let commands = body["commands"].as_array().unwrap();
        let mut results = Vec::new();
        for (index, command) in commands.iter().enumerate() {
            let name = command["command"].as_str().unwrap();
            let data = match name {
                "snapshot" => {
                    std::fs::write(command["path"].as_str().unwrap(), test_png(400, 100)).unwrap();
                    json!({"width": reported_width, "height": 100, "truncated": false})
                }
                "describe_workspace" => workspace_fixture(),
                _ => Value::Null,
            };
            results.push(json!({
                "command_index": index, "command": name, "status": "succeeded",
                "message": "ok", "data": data
            }));
        }
        (
            200,
            json!({
                "schema_version": AUTOMATION_SCHEMA_VERSION,
                "succeeded": true,
                "results": results,
                "telemetry": {"run_elapsed_ms": 5.0, "navigation": [{"panel_id": 1}]}
            }),
        )
    }

    fn status_response(schema_version: u32, instance: &str) -> (u16, Value) {
        (
            200,
            json!({
                "schema_version": schema_version,
                "control_schema_version": schema_version,
                "application_instance_id": instance,
                "process_id": 4242,
                "service": "rgv-local-control",
                "authentication": "bearer"
            }),
        )
    }

    fn capture_args(max_width: Option<u32>, include_telemetry: bool) -> CaptureArgs {
        CaptureArgs {
            scope: ToolSnapshotScope::VisibleWorkspace,
            max_width,
            include_telemetry,
            wait_until_settled: true,
            timeout_millis: DEFAULT_SETTLE_TIMEOUT_MILLIS,
        }
    }

    #[tokio::test]
    async fn capture_is_downscaled_deduplicated_and_its_temporary_file_removed() {
        let (address, worker) = fake_viewer(2, |url, body| {
            assert_eq!(url, "/v1/batches");
            assert_eq!(body["schema_version"], BATCH_SCHEMA_VERSION);
            capture_response(body, 400)
        });
        let root = tempfile::tempdir().unwrap();
        let server = test_server(address, root.path());

        let result = server
            .capture_view(Parameters(capture_args(Some(100), false)))
            .await;
        assert_eq!(result.is_error, Some(false));
        let structured = result.structured_content.clone().unwrap();
        assert_eq!(structured["width"], 100);
        assert_eq!(structured["height"], 25);
        assert_eq!(structured["source_width"], 400);
        assert_eq!(structured["source_height"], 100);
        assert_eq!(structured["downscaled"], true);
        let image = serde_json::to_value(&result.content[1]).unwrap();
        let png = BASE64_STANDARD
            .decode(image["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(structured["bytes"], png.len());
        assert_eq!(png_dimensions(&png), Some((100, 25)));
        // The workspace appears once (panels/loci), not again inside the report, and
        // telemetry is absent unless requested.
        assert_eq!(structured["loci"][0], "chr1:101-200");
        assert_eq!(structured["panels"][0]["id"], 1);
        assert_eq!(
            structured["report"]["results"][2]["command"],
            "describe_workspace"
        );
        assert_eq!(
            structured["report"]["results"][2]["data"],
            OMITTED_DATA_MARKER
        );
        assert_eq!(structured["report"]["results"][1]["data"]["width"], 400);
        assert!(structured["report"].get("telemetry").is_none());
        assert_eq!(structured.to_string().matches("\"tracks\"").count(), 0);
        assert_eq!(temporary_artifact_count(root.path()), 0);

        // The default bound never enlarges a narrower capture, and telemetry is opt-in.
        let result = server
            .capture_view(Parameters(capture_args(None, true)))
            .await;
        let structured = result.structured_content.unwrap();
        assert_eq!(structured["width"], 400);
        assert_eq!(structured["downscaled"], false);
        assert_eq!(structured["report"]["telemetry"]["run_elapsed_ms"], 5.0);
        assert_eq!(temporary_artifact_count(root.path()), 0);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn every_rejected_capture_discards_its_temporary_file() {
        // 1: metadata disagrees with the PNG. 2-3: window capture bound to another process.
        // 4-5: window capture from a viewer on an unsupported schema. 6: the viewer fails.
        let mut call = 0;
        let (address, worker) = fake_viewer(6, move |url, body| {
            call += 1;
            match (call, url) {
                (1, "/v1/batches") => capture_response(body, 999),
                (2 | 4, "/v1/batches") => capture_response(body, 400),
                (3, "/v1/status") => status_response(AUTOMATION_SCHEMA_VERSION, "instance-b"),
                (5, "/v1/status") => status_response(AUTOMATION_SCHEMA_VERSION + 1, "instance-a"),
                (6, "/v1/batches") => {
                    // The viewer may have written a partial file before failing.
                    let path = body["commands"][0]["path"].as_str().unwrap();
                    std::fs::write(path, b"partial").unwrap();
                    (
                        200,
                        json!({
                            "schema_version": AUTOMATION_SCHEMA_VERSION,
                            "succeeded": false,
                            "results": [{
                                "command_index": 0, "command": "snapshot", "status": "failed",
                                "error_code": "io",
                                "message": format!("could not write {path}: disk full")
                            }]
                        }),
                    )
                }
                other => panic!("unexpected request {other:?}"),
            }
        });
        let root = tempfile::tempdir().unwrap();
        let server = test_server(address, root.path());
        let window_args = || CaptureWindowArgs {
            max_width: None,
            include_telemetry: false,
            wait_until_settled: false,
            timeout_millis: DEFAULT_SETTLE_TIMEOUT_MILLIS,
        };
        let code = |result: &CallToolResult| {
            assert_eq!(result.is_error, Some(true));
            result.structured_content.as_ref().unwrap()["error"]["code"]
                .as_str()
                .unwrap()
                .to_owned()
        };

        let mismatch = server
            .capture_view(Parameters(capture_args(None, false)))
            .await;
        assert_eq!(code(&mismatch), "invalid_response");
        assert_eq!(temporary_artifact_count(root.path()), 0);

        let identity = server.capture_window(Parameters(window_args())).await;
        assert_eq!(code(&identity), "application_identity_mismatch");
        assert_eq!(temporary_artifact_count(root.path()), 0);

        let version = server.capture_window(Parameters(window_args())).await;
        assert_eq!(code(&version), "unsupported_version");
        assert_eq!(temporary_artifact_count(root.path()), 0);

        let mut args = capture_args(None, false);
        args.wait_until_settled = false;
        let failed = server.capture_view(Parameters(args)).await;
        assert_eq!(code(&failed), "io");
        let message = failed.structured_content.unwrap()["error"]["message"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            message.starts_with("RGV command snapshot failed: could not write [path]/rgv-mcp-")
        );
        assert!(message.ends_with(": disk full"));
        assert!(!message.contains(&root.path().display().to_string()));
        assert_eq!(temporary_artifact_count(root.path()), 0);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn status_rejects_an_unsupported_viewer_schema_and_reports_the_range() {
        let mut call = 0;
        let (address, worker) = fake_viewer(3, move |url, _| {
            assert_eq!(url, "/v1/status");
            call += 1;
            match call {
                1 => status_response(AUTOMATION_SCHEMA_VERSION, "instance-a"),
                2 => status_response(MINIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION - 1, "instance-a"),
                _ => (
                    401,
                    json!({"error": {"code": "unauthorized", "message": "Bearer secret was rejected for /Users/alice/rgv.secret"}}),
                ),
            }
        });
        let root = tempfile::tempdir().unwrap();
        let server = test_server(address, root.path());

        let compatible = server.status().await;
        assert_eq!(compatible.is_error, Some(false));
        let structured = compatible.structured_content.unwrap();
        assert_eq!(
            structured["supported_schema_versions"]["minimum"],
            MINIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION
        );
        assert_eq!(
            structured["supported_schema_versions"]["maximum"],
            MAXIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION
        );

        let unsupported = server.status().await;
        assert_eq!(unsupported.is_error, Some(true));
        let error = &unsupported.structured_content.unwrap()["error"];
        assert_eq!(error["code"], "unsupported_version");
        assert!(error["message"].as_str().unwrap().contains(&format!(
            "automation schema {}",
            MINIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION - 1
        )));

        let rejected = server.status().await;
        let error = &rejected.structured_content.unwrap()["error"];
        assert_eq!(error["code"], "unauthorized");
        let message = error["message"].as_str().unwrap();
        assert!(message.contains("was rejected for [path]/rgv.secret"));
        assert!(!message.contains("alice"));
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn export_inlines_pseudonymized_read_names_unless_explicitly_requested() {
        const EXPORT: &str = "# RGV 0.1.0 alignment (filtered view) export v1\ntrack_id\ttrack_name\tcontig\tstart\tend\tread_name\tflags\n3\ttumour\tchr1\t101\t250\tPATIENT7_READ_A\t0x0063\n3\ttumour\tchr1\t140\t290\tPATIENT7_READ_B\t0x0093\n3\ttumour\tchr1\t400\t550\tPATIENT7_READ_A\t0x0800\n";
        let (address, worker) = fake_viewer(2, |url, body| {
            assert_eq!(url, "/v1/batches");
            let command = &body["commands"][0];
            assert_eq!(command["command"], "export");
            std::fs::write(command["path"].as_str().unwrap(), EXPORT).unwrap();
            (
                200,
                json!({
                    "schema_version": AUTOMATION_SCHEMA_VERSION,
                    "succeeded": true,
                    "results": [{
                        "command_index": 0, "command": "export", "status": "succeeded",
                        "message": "Exported alignments", "data": {"records": 3}
                    }]
                }),
            )
        });
        let root = tempfile::tempdir().unwrap();
        let server = test_server(address, root.path());
        let args = |include_read_names| ExportArgs {
            path: "exports/reads.tsv".to_owned(),
            kind: ToolExportKind::Alignments,
            include_read_names,
            wait_until_settled: false,
            timeout_millis: DEFAULT_SETTLE_TIMEOUT_MILLIS,
        };

        let default = server.export(Parameters(args(false))).await;
        assert_eq!(default.is_error, Some(false));
        let structured = default.structured_content.unwrap();
        let inline = structured["inline_text"].as_str().unwrap();
        assert!(!structured.to_string().contains("PATIENT7"));
        assert_eq!(inline.matches("\tread_1\t").count(), 2);
        assert_eq!(inline.matches("\tread_2\t").count(), 1);
        assert_eq!(structured["inline_read_names"], "pseudonymized");
        assert_eq!(structured["inline_distinct_read_names_replaced"], 2);
        assert_eq!(structured["bytes"], EXPORT.len());
        // The durable artifact under the control root is the unmodified export.
        assert_eq!(
            std::fs::read_to_string(root.path().join("exports/reads.tsv")).unwrap(),
            EXPORT
        );

        let opted_in = server.export(Parameters(args(true))).await;
        let structured = opted_in.structured_content.unwrap();
        assert_eq!(structured["inline_text"], EXPORT);
        assert_eq!(structured["inline_read_names"], "included");
        worker.join().unwrap();
    }

    /// Collects `tool.path.to.property` for every schema property lacking a description.
    /// Internally tagged enum discriminators (single-constant strings) are exempt.
    fn undescribed_properties(schema: &Value, location: &str, missing: &mut Vec<String>) {
        match schema {
            Value::Object(object) => {
                if let Some(Value::Object(properties)) = object.get("properties") {
                    for (name, property) in properties {
                        let discriminator = property.get("const").is_some()
                            || property
                                .get("enum")
                                .and_then(Value::as_array)
                                .is_some_and(|values| values.len() == 1);
                        let described = property
                            .get("description")
                            .and_then(Value::as_str)
                            .is_some_and(|description| description.len() >= 12);
                        // `ToolAlignmentAuxValue` is adjacently tagged over newtype variants;
                        // its operand is described on each variant rather than on `value`.
                        let variant_described = location.contains("ToolAlignmentAuxValue")
                            && object.get("description").is_some();
                        if !discriminator && !described && !variant_described {
                            missing.push(format!("{location}.{name}"));
                        }
                    }
                }
                for (key, value) in object {
                    undescribed_properties(value, &format!("{location}/{key}"), missing);
                }
            }
            Value::Array(values) => {
                for value in values {
                    undescribed_properties(value, location, missing);
                }
            }
            _ => {}
        }
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
        let mut undescribed = Vec::new();
        for tool in &tools {
            let schema = serde_json::to_value(tool.input_schema.as_ref()).unwrap();
            undescribed_properties(&schema, &tool.name, &mut undescribed);
        }
        assert!(
            undescribed.is_empty(),
            "tool arguments without a schema description: {undescribed:#?}"
        );
        let schema_of = |name: &str| {
            serde_json::to_string(
                tools
                    .iter()
                    .find(|tool| tool.name == name)
                    .unwrap()
                    .input_schema
                    .as_ref(),
            )
            .unwrap()
        };
        assert!(schema_of("rgv_navigate").contains("one-based and inclusive"));
        assert!(schema_of("rgv_change_viewport").contains("Zero-based exclusive end"));
        assert!(schema_of("rgv_select").contains("Zero-based genomic position"));
        assert!(schema_of("rgv_find_alignments").contains("half-open"));
        assert!(schema_of("rgv_capture_view").contains("max_width"));
        assert!(schema_of("rgv_capture_window").contains("include_telemetry"));
        assert!(schema_of("rgv_export").contains("include_read_names"));
        assert!(schema_of("rgv_get_workspace").contains("include_telemetry"));
        let configure_input_schema = tools
            .iter()
            .find(|tool| tool.name == "rgv_configure")
            .unwrap()
            .input_schema
            .as_ref();
        let configure_schema = serde_json::to_string(configure_input_schema).unwrap();
        assert!(configure_schema.contains("show_reference"));
        assert!(configure_schema.contains("window_size"));
        assert!(configure_schema.contains("interface_scale_percent"));
        assert!(configure_schema.contains("properties_panel_expanded"));
        assert!(configure_schema.contains("properties_panel_width"));
        assert!(configure_schema.contains("properties_panel_resize_active"));
        assert!(configure_schema.contains("vertical_scroll_points"));
        assert!(configure_schema.contains("presentation_section_expanded"));
        assert!(configure_schema.contains("presentation_node_visible"));
        assert!(configure_schema.contains("presentation_group_expanded"));
        assert!(configure_schema.contains("presentation_composite_visible"));
        assert!(configure_schema.contains("read_panel_scroll_points"));
        assert!(configure_schema.contains("read_panel_density"));
        assert!(configure_schema.contains("alignment_color_mode"));
        assert!(configure_schema.contains("read_base_display_mode"));
        assert!(configure_schema.contains("^[A-Za-z][A-Za-z0-9]$"));
        let configure_properties = configure_input_schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("configure input schema should expose its top-level properties");
        assert!(!configure_properties.contains_key("scope"));
        assert!(!configure_properties.contains_key("key"));
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

    /// The wire identifiers (`enum` values, tag constants or externally-tagged keys) of the
    /// variants a schema publishes.
    fn schema_variant_keys(schema: &Value) -> std::collections::BTreeSet<String> {
        fn entry_keys(entry: &Value, keys: &mut std::collections::BTreeSet<String>) {
            if let Some(value) = entry.get("const").and_then(Value::as_str) {
                keys.insert(value.to_owned());
                return;
            }
            if let Some(values) = entry.get("enum").and_then(Value::as_array) {
                keys.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
                return;
            }
            let Some(properties) = entry.get("properties").and_then(Value::as_object) else {
                return;
            };
            let tags = properties
                .values()
                .filter_map(|property| {
                    property.get("const").and_then(Value::as_str).or_else(|| {
                        property
                            .get("enum")
                            .and_then(Value::as_array)
                            .filter(|values| values.len() == 1)
                            .and_then(|values| values[0].as_str())
                    })
                })
                .collect::<Vec<_>>();
            if let [tag] = tags.as_slice() {
                keys.insert((*tag).to_owned());
            } else if properties.len() == 1 {
                keys.extend(properties.keys().cloned());
            }
        }
        let mut keys = std::collections::BTreeSet::new();
        entry_keys(schema, &mut keys);
        for combinator in ["oneOf", "anyOf"] {
            for entry in schema
                .get(combinator)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                entry_keys(entry, &mut keys);
            }
        }
        // Adapters may spell a variant differently from core (`local` for `Local`); what must
        // not drift is the set of variants, so compare identifiers without case or separators.
        keys.into_iter()
            .map(|key| {
                key.chars()
                    .filter(char::is_ascii_alphanumeric)
                    .map(|character| character.to_ascii_lowercase())
                    .collect()
            })
            .collect()
    }

    /// OPS-6: the hand-written `Tool*` argument enums mirror `rgv-core` wire types. With the core
    /// `schema` feature both sides publish a schema, so the mirror can be checked mechanically:
    /// every variant a tool offers must exist in core under the same wire identifier, and every
    /// core variant must either be offered or be listed here as deliberately withheld. Adding,
    /// renaming or removing a core variant therefore fails this test until the tool is updated.
    #[allow(clippy::too_many_lines)]
    #[test]
    fn tool_argument_enums_cannot_drift_from_the_core_wire_types() {
        let mut failures = Vec::<String>::new();
        macro_rules! mirror {
            ($tool:ty, $core:ty, [$($withheld:literal),* $(,)?]) => {
                mirror!($tool, $core, [$($withheld),*], adapter_only: [])
            };
            ($tool:ty, $core:ty, [$($withheld:literal),* $(,)?], adapter_only: [$($adapter:literal),* $(,)?]) => {{
                let tool = schema_variant_keys(
                    &serde_json::to_value(schemars::schema_for!($tool)).unwrap(),
                );
                let core = schema_variant_keys(
                    &serde_json::to_value(schemars::schema_for!($core)).unwrap(),
                );
                let withheld = [$($withheld),*]
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<std::collections::BTreeSet<String>>();
                assert!(!tool.is_empty(), "{} publishes no variants", stringify!($tool));
                assert!(!core.is_empty(), "{} publishes no variants", stringify!($core));
                let adapter_only = [$($adapter),*]
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<std::collections::BTreeSet<String>>();
                let unknown = tool
                    .difference(&core)
                    .filter(|key| !adapter_only.contains(*key))
                    .collect::<Vec<_>>();
                if !unknown.is_empty() {
                    failures.push(format!(
                        "{} offers variants that {} does not define: {unknown:?}",
                        stringify!($tool),
                        stringify!($core)
                    ));
                }
                let unexposed = core
                    .difference(&tool)
                    .filter(|key| !withheld.contains(*key))
                    .collect::<Vec<_>>();
                if !unexposed.is_empty() {
                    failures.push(format!(
                        "{} defines variants that {} neither offers nor withholds: {unexposed:?}",
                        stringify!($core),
                        stringify!($tool)
                    ));
                }
                let stale = withheld
                    .difference(&core)
                    .chain(adapter_only.difference(&tool))
                    .collect::<Vec<_>>();
                if !stale.is_empty() {
                    failures.push(format!(
                        "{} withholds variants that {} no longer defines: {stale:?}",
                        stringify!($tool),
                        stringify!($core)
                    ));
                }
            }};
        }
        mirror!(
            ToolViewportOperation,
            rgv_core::AutomationViewportOperation,
            []
        );
        mirror!(ToolSource, rgv_core::TrackSource, []);
        mirror!(ToolTrackKind, rgv_core::TrackKind, []);
        mirror!(ToolTrackChannel, rgv_core::TrackChannel, []);
        mirror!(ToolPresentationNode, rgv_core::PresentationNodeId, []);
        mirror!(
            ToolAlignmentPredicateScope,
            rgv_core::AlignmentPredicateScope,
            []
        );
        mirror!(
            ToolAlignmentFlagPredicate,
            rgv_core::AlignmentFlagPredicate,
            []
        );
        mirror!(
            ToolAlignmentCigarEventPredicate,
            rgv_core::AlignmentCigarEventPredicate,
            []
        );
        mirror!(
            ToolAlignmentAuxComparison,
            rgv_core::AlignmentAuxComparison,
            []
        );
        // Adapter: core stores a float operand as its IEEE-754 bits (`float_bits`) so the
        // predicate stays `Eq`/hashable; the tool accepts an ordinary JSON number.
        mirror!(
            ToolAlignmentAuxValue,
            rgv_core::AlignmentAuxFilterValue,
            ["floatbits"],
            adapter_only: ["float"]
        );
        mirror!(ToolAlignmentPredicate, rgv_core::AlignmentViewPredicate, []);
        // Adapter: `alignment_color_tag` is a tool-side variant that carries the SAM tag as a
        // validated string and becomes `alignment_color_mode` with core's `tag` colour mode.
        // The five withheld core settings are not offered as typed MCP settings; they remain
        // reachable only through `rgv_execute_batch`.
        mirror!(
            ToolSetting,
            rgv_core::AutomationSetting,
            [
                "completereadlimit",
                "readbrowsingbufferpercent",
                "readstackingmode",
                "rebuildstablereadarchitecture",
                "totalcachebudgetmib",
            ],
            adapter_only: ["alignmentcolortag"]
        );
        mirror!(
            ToolCoverageScaleTransform,
            rgv_core::CoverageScaleTransform,
            []
        );
        mirror!(
            ToolOverlappingMatePolicy,
            rgv_core::OverlappingMatePolicy,
            []
        );
        mirror!(ToolReadBaseDisplayMode, rgv_core::ReadBaseDisplayMode, []);
        mirror!(
            ToolModifiedBaseColorMode,
            rgv_core::ModifiedBaseColorMode,
            []
        );
        // `tag` carries a payload and is offered as the `alignment_color_tag` setting instead; the
        // platform-specific colour modes are not offered as typed MCP values.
        mirror!(
            ToolAlignmentColorMode,
            rgv_core::AlignmentColorMode,
            ["bisulfite", "sbx", "smrtkinetics", "tag", "ultima"]
        );
        mirror!(ToolGroupMode, rgv_core::AlignmentGroupMode, []);
        mirror!(ToolSortMode, rgv_core::AlignmentSortMode, []);
        mirror!(ToolDirection, rgv_core::AlignmentOrderDirection, []);
        mirror!(ToolSelection, rgv_core::AutomationSelection, []);
        mirror!(
            ToolSelectionGranularity,
            rgv_core::AutomationSelectionGranularity,
            []
        );
        mirror!(
            ToolReviewNavigation,
            rgv_core::AutomationReviewNavigation,
            []
        );
        mirror!(
            ToolHistoryNavigation,
            rgv_core::AutomationHistoryNavigation,
            []
        );
        mirror!(
            ToolAlignmentSelector,
            rgv_core::AutomationAlignmentSelector,
            []
        );
        mirror!(ToolAlignmentState, rgv_core::AutomationAlignmentState, []);
        mirror!(ToolAlignmentReveal, rgv_core::AutomationAlignmentReveal, []);
        mirror!(
            ToolAlignmentRelationship,
            rgv_core::AutomationAlignmentRelationship,
            []
        );
        mirror!(
            ToolNavigationDestination,
            rgv_core::AutomationNavigationDestination,
            []
        );
        // `native_window` is the separate `rgv_capture_window` tool.
        mirror!(
            ToolSnapshotScope,
            rgv_core::AutomationSnapshotScope,
            ["nativewindow"]
        );
        mirror!(ToolExportKind, rgv_core::AutomationExportKind, []);
        mirror!(
            ToolUnsavedChangesPolicy,
            rgv_core::AutomationUnsavedChangesPolicy,
            []
        );
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
