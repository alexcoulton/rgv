use super::super::*;

impl Viewer {
    pub(crate) fn request_track_query(&mut self) {
        self.panels.active_mut().pending_queries.tracks = self.track_engine.is_some();
        self.dispatch_track_query();
    }

    pub(crate) fn request_reference_query(&mut self) {
        let at_base_detail = self.panels.active_mut().viewport.detail_level() == DetailLevel::Bases;
        self.panels.active_mut().pending_queries.reference =
            self.reference_engine.is_some() && at_base_detail;
        self.dispatch_reference_query();
    }

    pub(crate) fn dispatch_reference_query(&mut self) {
        if !self.panels.active().pending_queries.reference {
            return;
        }
        let Some(engine) = &self.reference_engine else {
            self.panels.active_mut().pending_queries.reference = false;
            return;
        };
        let request = self.panels.active().request();
        match engine.try_query(
            request,
            self.panels.active_mut().generation_clock.clone(),
            self.panels.active_mut().contig_name.clone(),
            self.panels.active_mut().viewport.interval(),
        ) {
            Ok(()) => self.panels.active_mut().pending_queries.reference = false,
            Err(EngineQueueError::Full) => {
                "Reference queue busy; retrying the latest locus…"
                    .clone_into(&mut self.data_status);
            }
            Err(EngineQueueError::Disconnected) => {
                self.panels.active_mut().pending_queries.reference = false;
                self.data_status = EngineQueueError::Disconnected.to_string();
            }
        }
    }

    pub(crate) fn request_all_panel_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            self.panels.activate(id);
            self.request_alignment_query();
            self.request_reference_query();
            self.request_track_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn request_all_alignment_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            self.panels.activate(id);
            self.request_alignment_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn request_all_auxiliary_track_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            self.panels.activate(id);
            self.request_track_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn invalidate_and_request_all_auxiliary_track_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            self.panels.activate(id);
            self.panels.active().generation_clock.advance();
            self.request_reference_query();
            self.request_track_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn dispatch_track_query(&mut self) {
        if !self.panels.active().pending_queries.tracks {
            return;
        }
        let Some(engine) = &self.track_engine else {
            self.panels.active_mut().pending_queries.tracks = false;
            return;
        };
        let interval = self.panels.active().viewport.interval();
        let request = self.panels.active().request();
        let track_ids = auxiliary_query_track_ids(self.tracks.tracks());
        let queried = track_ids.iter().copied().collect::<HashSet<_>>();
        let variant_settings = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Variant && queried.contains(&track.id))
            .map(|track| (track.id, track.variant.clone()))
            .collect();
        let interaction_settings = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Interaction && queried.contains(&track.id))
            .map(|track| (track.id, track.interaction.clone()))
            .collect();
        let copy_number_settings = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::CopyNumber && queried.contains(&track.id))
            .map(|track| (track.id, track.copy_number.clone()))
            .collect();
        match engine.try_query_selected_with_settings(
            request,
            self.panels.active_mut().generation_clock.clone(),
            self.panels.active_mut().contig_name.clone(),
            interval.start(),
            interval.end(),
            track_ids,
            variant_settings,
            interaction_settings,
            copy_number_settings,
        ) {
            Ok(()) => self.panels.active_mut().pending_queries.tracks = false,
            Err(EngineQueueError::Full) => {
                "Track queue busy; retrying the latest locus…".clone_into(&mut self.data_status);
            }
            Err(EngineQueueError::Disconnected) => {
                self.panels.active_mut().pending_queries.tracks = false;
                self.data_status = EngineQueueError::Disconnected.to_string();
            }
        }
    }

    pub(crate) fn retry_pending_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            let alignment_pending = self.alignment_tracks.iter().any(|track| {
                track
                    .projection(id)
                    .is_some_and(|projection| projection.pending_query)
            });
            self.panels.activate(id);
            self.panels.active_mut().pending_queries.alignment |= alignment_pending;
            self.dispatch_alignment_query();
            self.dispatch_reference_query();
            self.dispatch_track_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn poll_background_work(&mut self) {
        self.poll_reference_events();
        self.poll_alignment_events();
        self.poll_track_events();
        self.poll_alignment_retirement_tasks();
        self.poll_session_load_tasks();
        self.poll_session_save_tasks();
        self.poll_export_tasks();
        self.poll_workspace_list_import_tasks();
        self.retry_pending_queries();
        if self.display_dirty {
            self.window.request_redraw();
        }
    }

    pub(crate) fn poll_alignment_retirement_tasks(&mut self) {
        let mut index = 0;
        while index < self.alignment_retirement_tasks.len() {
            if !self.alignment_retirement_tasks[index].is_finished() {
                index += 1;
                continue;
            }
            let task = self.alignment_retirement_tasks.remove(index);
            if task.join().is_err() {
                tracing::warn!("alignment reaper panicked");
            }
        }
    }

    pub(crate) fn finish_alignment_retirement_tasks(&mut self) {
        for task in self.alignment_retirement_tasks.drain(..) {
            if task.join().is_err() {
                tracing::warn!("alignment reaper panicked during shutdown");
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn poll_reference_events(&mut self) {
        let mut events = Vec::new();
        if let Some(engine) = &self.reference_engine {
            loop {
                match engine.try_receive() {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => break,
                    Err(error) => {
                        self.data_status = error.to_string();
                        break;
                    }
                }
            }
        }
        if events.is_empty() {
            return;
        }
        let active_panel_id = self.panels.active().id;
        for event in events {
            match event {
                ReferenceEngineEvent::Opened {
                    dictionary,
                    remote_io_stats,
                } => {
                    self.reference_open_error = None;
                    self.reference_remote_io_stats = remote_io_stats;
                    self.reference_dictionary = Some(dictionary.clone());
                    if self.primary_alignment_dictionary().is_none() {
                        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
                        for id in ids {
                            let (requested, pixel_width) = self
                                .panels
                                .get(id)
                                .map(|panel| {
                                    (panel.requested_locus.clone(), panel.viewport.pixel_width())
                                })
                                .expect("panel id came from the panel collection");
                            match resolve_initial_locus(
                                &dictionary,
                                &self.contig_aliases,
                                requested.as_deref(),
                            )
                            .and_then(|(interval, name, length)| {
                                Ok((
                                    Viewport::new(interval, pixel_width)?,
                                    interval,
                                    name,
                                    length,
                                ))
                            }) {
                                Ok((viewport, interval, name, length)) => {
                                    let panel = self
                                        .panels
                                        .get_mut(id)
                                        .expect("panel id came from the panel collection");
                                    panel.generation_clock.advance();
                                    panel.viewport = viewport;
                                    panel.contig_name = name;
                                    panel.contig_length = length;
                                    panel.home_interval = interval;
                                    panel.navigation_history = NavigationHistory::new(interval);
                                    panel.vertical_scroll_points = 0.0;
                                    panel.reference_slice = None;
                                }
                                Err(error) if id == active_panel_id => {
                                    self.data_status = error.to_string();
                                }
                                Err(_) => {}
                            }
                        }
                        self.assembly = load_assembly(
                            &self.assembly_name,
                            dictionary,
                            self.cytoband_path.as_deref(),
                        );
                    }
                    self.synthetic = false;
                    if !self.has_alignment_engine() {
                        let source = self.reference_source.as_ref().map_or_else(
                            || "reference genome".to_owned(),
                            TrackSource::redacted_label,
                        );
                        self.data_status = format!("Opened {source}");
                    }
                    self.refresh_active_panel_context();
                    self.request_all_panel_queries();
                    self.display_dirty = true;
                }
                ReferenceEngineEvent::QueryCompleted {
                    request,
                    slice,
                    remote_io_stats,
                } => {
                    self.reference_remote_io_stats = remote_io_stats;
                    let is_active = request.panel_id() == active_panel_id;
                    if let Some(panel) = self.panels.get_mut(request.panel_id())
                        && panel.accepts(request)
                        && slice.interval.contains_interval(panel.viewport.interval())
                    {
                        panel.reference_slice = Some(slice);
                        panel.reference_settled_generation = Some(request.generation().get());
                        self.display_dirty = true;
                        if is_active && !self.has_alignment_engine() {
                            "Reference sequence loaded".clone_into(&mut self.data_status);
                        }
                    }
                }
                ReferenceEngineEvent::Error { request, message } => {
                    if request.is_none() {
                        self.reference_open_error = Some(message.clone());
                    }
                    let current = request.is_none_or(|request| {
                        self.panels
                            .get(request.panel_id())
                            .is_some_and(|panel| panel.accepts(request))
                    });
                    if current {
                        if let Some(request) = request
                            && let Some(panel) = self.panels.get_mut(request.panel_id())
                        {
                            panel.reference_settled_generation = Some(request.generation().get());
                        }
                        self.data_status = message;
                    }
                }
            }
        }
        self.window.request_redraw();
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn poll_alignment_events(&mut self) {
        let mut events = Vec::new();
        let active_panel_id = self.panels.active().id;
        let primary_id = self.alignment_tracks.first().map(|track| track.config.id);
        for track in &mut self.alignment_tracks {
            let id = track.config.id;
            loop {
                match track.engine.try_receive() {
                    Ok(Some(event)) => events.push((id, event)),
                    Ok(None) => break,
                    Err(error) => {
                        track.lifecycle = AlignmentTrackLifecycle::Failed(error.to_string());
                        if Some(id) == primary_id {
                            self.data_status = error.to_string();
                        }
                        break;
                    }
                }
            }
        }
        let had_events = !events.is_empty();
        for (track_id, event) in events {
            let mut event_is_current = true;
            if let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == track_id)
            {
                match &event {
                    AlignmentEngineEvent::Opened {
                        dictionary,
                        index_overview,
                    } => {
                        track.lifecycle = AlignmentTrackLifecycle::Ready;
                        track.dictionary = Some(dictionary.clone());
                        track.index_overview.clone_from(index_overview);
                        for projection in track.projections.values_mut() {
                            projection.pending_query = track.config.visible;
                        }
                    }
                    AlignmentEngineEvent::QueryCompleted {
                        request,
                        query,
                        cache_stats,
                        remote_io_stats,
                        ..
                    } => {
                        let projection = track.projection_mut(request.panel_id());
                        event_is_current = projection.clock.is_current(request.generation());
                        if event_is_current {
                            projection.query_stats = Some(query.stats);
                            projection.current_query = Some(Arc::clone(query));
                            projection.unavailable = None;
                            projection.settled_generation = Some(request.generation().get());
                            track.cache_stats = Some(*cache_stats);
                            track.remote_io_stats = *remote_io_stats;
                        }
                    }
                    AlignmentEngineEvent::Error { request, message } => {
                        event_is_current = request.is_none_or(|request| {
                            track
                                .projection(request.panel_id())
                                .is_some_and(|projection| {
                                    projection.clock.is_current(request.generation())
                                })
                        });
                        if event_is_current {
                            track.lifecycle = AlignmentTrackLifecycle::Failed(message.clone());
                        }
                    }
                    AlignmentEngineEvent::CacheCleared { cache_stats } => {
                        track.cache_stats = Some(*cache_stats);
                    }
                }
            }
            if matches!(event, AlignmentEngineEvent::Opened { .. }) {
                self.panels.active_mut().pending_queries.alignment = true;
            }
            if event_is_current && matches!(event, AlignmentEngineEvent::QueryCompleted { .. }) {
                // Results for inactive panels and secondary tracks still alter
                // the composite workspace even when they do not change the
                // active status line.
                self.display_dirty = true;
            }
            if Some(track_id) != primary_id {
                if event_is_current {
                    match &event {
                        AlignmentEngineEvent::QueryCompleted { request, query, .. } => {
                            self.resolve_pending_read_selection(
                                request.panel_id(),
                                track_id,
                                query,
                            );
                        }
                        AlignmentEngineEvent::Error { request, message }
                            if request
                                .is_none_or(|request| request.panel_id() == active_panel_id) =>
                        {
                            let name = self
                                .alignment_tracks
                                .iter()
                                .find(|track| track.config.id == track_id)
                                .map_or("Alignment track", |track| track.config.name.as_str());
                            self.data_status = format!("{name}: {message}");
                        }
                        _ => {}
                    }
                }
                continue;
            }
            match event {
                AlignmentEngineEvent::Opened { dictionary, .. } => {
                    if let Some(source) = self.primary_alignment_source().cloned() {
                        self.settings.remember_alignment(&source);
                        self.persist_settings();
                    }
                    let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
                    for id in ids {
                        let (requested, pixel_width) = self
                            .panels
                            .get(id)
                            .map(|panel| {
                                (panel.requested_locus.clone(), panel.viewport.pixel_width())
                            })
                            .expect("panel id came from the panel collection");
                        match resolve_initial_locus(
                            &dictionary,
                            &self.contig_aliases,
                            requested.as_deref(),
                        )
                        .and_then(|(interval, name, length)| {
                            Ok((
                                Viewport::new(interval, pixel_width)?,
                                interval,
                                name,
                                length,
                            ))
                        }) {
                            Ok((viewport, interval, name, length)) => {
                                let panel = self
                                    .panels
                                    .get_mut(id)
                                    .expect("panel id came from the panel collection");
                                panel.viewport = viewport;
                                panel.contig_name = name;
                                panel.contig_length = length;
                                panel.home_interval = interval;
                                panel.navigation_history = NavigationHistory::new(interval);
                                panel.vertical_scroll_points = 0.0;
                            }
                            Err(error) if id == active_panel_id => {
                                self.data_status = error.to_string();
                            }
                            Err(_) => {}
                        }
                    }
                    self.assembly = load_assembly(
                        &self.assembly_name,
                        dictionary.clone(),
                        self.cytoband_path.as_deref(),
                    );
                    self.request_all_panel_queries();
                }
                AlignmentEngineEvent::QueryCompleted {
                    request,
                    query,
                    cache_hit,
                    cache_stats,
                    remote_io_stats,
                } if event_is_current => {
                    let is_active = request.panel_id() == active_panel_id;
                    if let Some(panel) = self.panels.get_mut(request.panel_id()) {
                        let has_pending_selection = panel
                            .pending_read_selection
                            .as_ref()
                            .is_some_and(|(pending_track, _)| *pending_track == track_id);
                        if !has_pending_selection {
                            panel.selected_record = None;
                            panel.selected_alignment_track = None;
                            panel.selected_read_event = None;
                        }
                    }
                    self.resolve_pending_read_selection(request.panel_id(), track_id, &query);
                    if !is_active {
                        continue;
                    }
                    let cache_label = if cache_hit { "cache hit · " } else { "" };
                    self.data_status = if query.stats.truncated {
                        format!(
                            "{cache_label}safety cap loaded {} / {} alignments",
                            query.stats.records_emitted, query.stats.records_eligible
                        )
                    } else {
                        format!("{cache_label}{} alignments", query.stats.records_emitted)
                    };
                    self.rebuild_genomic_display();
                    self.display_dirty = true;
                }
                AlignmentEngineEvent::Error { request, message }
                    if event_is_current
                        && request.is_none_or(|request| request.panel_id() == active_panel_id) =>
                {
                    self.data_status = message;
                }
                AlignmentEngineEvent::CacheCleared { .. } => {
                    "Alignment cache cleared".clone_into(&mut self.data_status);
                }
                AlignmentEngineEvent::QueryCompleted { .. }
                | AlignmentEngineEvent::Error { .. } => {}
            }
        }
        if had_events {
            self.window.request_redraw();
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn poll_track_events(&mut self) {
        let mut events = Vec::new();
        if let Some(engine) = &self.track_engine {
            loop {
                match engine.try_receive() {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => break,
                    Err(error) => {
                        self.data_status = error.to_string();
                        break;
                    }
                }
            }
        }
        let had_events = !events.is_empty();
        for event in events {
            if let TrackEngineEvent::Error {
                track: Some(track_id),
                message,
                ..
            } = &event
                && self.automation_auxiliary_open_state.contains_key(track_id)
            {
                self.automation_auxiliary_open_state.insert(
                    *track_id,
                    AutomationAuxiliaryOpenState::Failed(message.clone()),
                );
            }
            match event {
                TrackEngineEvent::Ready { opened, failed } => {
                    tracing::info!(opened, failed, "auxiliary tracks opened");
                    let pending = self
                        .automation_auxiliary_open_state
                        .iter()
                        .filter_map(|(id, state)| {
                            (state == &AutomationAuxiliaryOpenState::Pending).then_some(*id)
                        })
                        .collect::<Vec<_>>();
                    if pending.len() == 1 {
                        let state = if failed == 0 && opened > 0 {
                            AutomationAuxiliaryOpenState::Ready
                        } else {
                            AutomationAuxiliaryOpenState::Failed(
                                "The auxiliary source could not be opened".to_owned(),
                            )
                        };
                        self.automation_auxiliary_open_state
                            .insert(pending[0], state);
                    }
                    if !self.synthetic {
                        self.request_all_panel_queries();
                    }
                }
                TrackEngineEvent::TrackAdded { id } => {
                    if self.automation_auxiliary_open_state.contains_key(&id) {
                        self.automation_auxiliary_open_state
                            .insert(id, AutomationAuxiliaryOpenState::Ready);
                    }
                    self.request_all_panel_queries();
                }
                TrackEngineEvent::TracksAdded { ids } => {
                    for id in ids {
                        if self.automation_auxiliary_open_state.contains_key(&id) {
                            self.automation_auxiliary_open_state
                                .insert(id, AutomationAuxiliaryOpenState::Ready);
                        }
                    }
                    self.request_all_panel_queries();
                }
                TrackEngineEvent::TracksRemoved { ids } => {
                    let removed = ids.into_iter().collect::<HashSet<_>>();
                    for panel in self.panels.iter_mut() {
                        clear_removed_auxiliary_panel_state(panel, &removed);
                    }
                    self.annotation_search_results
                        .hits
                        .retain(|hit| !removed.contains(&hit.track_id));
                    self.display_dirty = true;
                }
                TrackEngineEvent::TdfChannelsDiscovered {
                    id,
                    track_names,
                    selected_index,
                } => {
                    if let Err(error) = self.expand_tdf_channels(id, &track_names, selected_index) {
                        self.data_status = error.to_string();
                    }
                }
                TrackEngineEvent::QueryCompleted { request, tracks } => {
                    let is_active = request.panel_id() == self.panels.active().id;
                    if let Some(panel) = self.panels.get_mut(request.panel_id())
                        && panel.accepts(request)
                    {
                        panel.track_snapshots = tracks;
                        panel.track_snapshot_interval = Some(panel.viewport.interval());
                        panel.track_settled_generation = Some(request.generation().get());
                        panel.selected_feature = None;
                        panel.selected_variant = None;
                        panel.selected_gwas = None;
                        panel.selected_quantitative = None;
                        panel.selected_copy_number = None;
                        panel.selected_mutation = None;
                        panel.selected_interaction = None;
                        self.display_dirty = true;
                        if is_active {
                            self.rebuild_genomic_display();
                        }
                    }
                }
                TrackEngineEvent::SearchCompleted {
                    generation,
                    results,
                } if generation == self.annotation_search_generation => {
                    let pending = self
                        .pending_annotation_navigation
                        .take()
                        .filter(|query| query.eq_ignore_ascii_case(results.query.trim()));
                    self.annotation_search_results = results;
                    if let Some(query) = pending {
                        self.submit_annotation_search_results(&query);
                    }
                    self.display_dirty = true;
                }
                TrackEngineEvent::AdjacentFeatureCompleted { generation, hit }
                    if generation == self.annotation_navigation_generation =>
                {
                    if let Some(hit) = hit {
                        self.navigate_to_annotation_hit(&hit);
                    } else {
                        "No further annotation feature on this contig"
                            .clone_into(&mut self.data_status);
                    }
                }
                TrackEngineEvent::Error {
                    request,
                    track: _,
                    message,
                } if request.is_none()
                    || request.is_some_and(|request| {
                        self.panels
                            .get(request.panel_id())
                            .is_some_and(|panel| panel.accepts(request))
                            && request.panel_id() == self.panels.active_mut().id
                    }) =>
                {
                    self.data_status = message;
                }
                TrackEngineEvent::SearchCompleted { .. }
                | TrackEngineEvent::AdjacentFeatureCompleted { .. }
                | TrackEngineEvent::Error { .. } => {}
            }
        }
        if had_events {
            self.window.request_redraw();
        }
    }
}
