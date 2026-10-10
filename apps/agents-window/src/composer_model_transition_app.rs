//! Local model transitions preserve the journal and portable conversation data.
//! A checkpoint is plain text, so supported v1 summaries can be re-bound to a
//! new route without generating a summary or sending a request to a provider.

use super::*;

fn portable_checkpoint_format(value: &str) -> bool {
    matches!(
        value,
        "responses-v1" | "chat-completions-v1" | "anthropic-messages-v1"
    )
}

pub(super) fn migrate_checkpoint_for_model(
    data_root: &Path,
    store: &SessionStore,
    lease: &openide_core::NativeSessionLease,
    target: &CheckpointCompatibility,
) -> Result<Option<AgentEvent>, String> {
    let stored = store
        .load(lease.session_id())
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "La conversación ya no está disponible".to_owned())?;
    let replay = replay_stored_native(data_root, &stored)?;
    let Some(prior) = replay.checkpoint else {
        return Ok(None);
    };
    if prior.compatibility == *target {
        return Ok(None);
    }
    if !replay.can_start_new_turn {
        return Err("Finaliza o resuelve el turno pendiente antes de cambiar el modelo de esta conversación.".into());
    }
    if !portable_checkpoint_format(&prior.compatibility.context_format_key)
        || !portable_checkpoint_format(&target.context_format_key)
    {
        return Err("Este contexto usa un formato que no se puede trasladar. Conserva el modelo anterior o crea una nueva tarea para usar el modelo elegido.".into());
    }
    let mut migrated = prior;
    migrated.checkpoint_id = format!("route-migration-{}", Uuid::new_v4());
    migrated.compatibility = target.clone();
    // The old model's usage/limits are not measurements of the new model.
    migrated.budget = openide_core::CheckpointBudgetSnapshot::unverified(
        migrated.budget.estimated_input_tokens,
        None,
    );
    let checkpoints = CheckpointFileStore::new(data_root.join("sessions"))
        .map_err(|error| format!("No se pudo abrir el contexto guardado: {error}"))?;
    let reference = checkpoints.write_candidate(&migrated).map_err(|error| {
        format!("No se pudo preparar el contexto para el modelo elegido: {error}")
    })?;
    let sequence = replay
        .last_sequence
        .checked_add(1)
        .ok_or_else(|| "La conversación agotó su secuencia de eventos".to_owned())?;
    let marker = AgentEvent::new(
        lease.session_id(),
        sequence,
        AgentEventKind::CheckpointCommitted { reference },
    );
    store
        .append_native_event(&marker)
        .map_err(|error| format!("No se pudo confirmar el cambio de contexto: {error}"))?;
    // Reload through the same integrity checks used on restart and resume.
    let verified = store
        .load(lease.session_id())
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "La conversación ya no está disponible".to_owned())?;
    let verified = replay_stored_native(data_root, &verified)?;
    if verified.checkpoint.as_ref() != Some(&migrated) || !verified.can_start_new_turn {
        return Err("No se pudo verificar el contexto del modelo elegido".into());
    }
    Ok(Some(marker))
}

impl AgentsWindow {
    pub(super) fn prepare_composer_model_transition(
        &mut self,
        route: &composer_selection::ModelRouteOption,
    ) -> Result<(), String> {
        let Some(owner) = self.view.selected_session.clone() else {
            return Ok(());
        };
        let store = self
            .store
            .clone()
            .ok_or_else(|| "El historial no está disponible".to_owned())?;
        let stored = store
            .load(&owner)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "La conversación ya no está disponible".to_owned())?;
        let Some(prior) = stored
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                AgentEventKind::CheckpointCommitted { reference } => Some(&reference.compatibility),
                _ => None,
            })
        else {
            return Ok(());
        };
        let adapter = native_provider::adapter_for_model_at(
            &self.data_root,
            &route.provider_id,
            &route.model_id,
        )?
        .ok_or_else(|| "El proveedor elegido ya no está disponible".to_owned())?;
        let target =
            native_checkpoint_compatibility(&route.provider_id, &route.model_id, adapter.wire_api);
        if prior == &target {
            return Ok(());
        }
        let lease = store.try_acquire_native_lease(&owner).map_err(|error| {
            format!("Espera a que termine la conversación antes de cambiar el modelo: {error}")
        })?;
        if let Some(marker) =
            migrate_checkpoint_for_model(&self.data_root, &store, &lease, &target)?
        {
            self.last_sequence = marker.sequence;
            self.projection.apply_event(&marker);
            self.publish_transcript();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint_fixture(
        root: &Path,
        format: &str,
    ) -> (AgentsWindow, openide_core::CheckpointReference) {
        let (mut app, _credential) =
            super::super::tests::native_composer_test_fixture(&root.join("data"));
        let store = app.store.as_ref().unwrap();
        let session = store.create(root, "native").unwrap();
        let prompt = "Conserva el resultado de la lectura";
        let model = app.view.model.clone();
        let _lease = store.try_acquire_native_lease(&session.id).unwrap();
        store
            .admit_native_turn(&session.id, "first", prompt, &model, TurnMode::Build)
            .unwrap();
        for (index, kind) in [
            AgentEventKind::TurnStarted {
                prompt: prompt.into(),
            },
            AgentEventKind::ModelRequestStarted {
                step: 1,
                model: model.clone(),
            },
            AgentEventKind::ModelResponseCommitted {
                step: 1,
                assistant: AgentMessage::assistant("El archivo contiene un servidor Rust", vec![]),
                response_id: Some("response-old".into()),
                usage: None,
            },
            AgentEventKind::TurnCompleted {
                response: "El archivo contiene un servidor Rust".into(),
                steps: 1,
            },
        ]
        .into_iter()
        .enumerate()
        {
            store
                .append_native_event(&AgentEvent::new(&session.id, index as u64 + 2, kind))
                .unwrap();
        }
        let checkpoint = openide_core::Checkpoint {
            session_id: session.id.clone(),
            checkpoint_id: "original-context".into(),
            covered_event_sequence: 5,
            compatibility: CheckpointCompatibility {
                provider: app.view.native_provider_id.clone(),
                model,
                context_format_key: format.into(),
            },
            summary: "Se inspeccionó src/server.rs. Conserva sus rutas y pruebas.".into(),
            retained_user_messages: vec![openide_core::RetainedUserMessage {
                event_sequence: 1,
                content: prompt.into(),
            }],
            budget: openide_core::CheckpointBudgetSnapshot::unverified(100, None),
        };
        let checkpoints = CheckpointFileStore::new(app.data_root.join("sessions")).unwrap();
        let reference = checkpoints.write_candidate(&checkpoint).unwrap();
        store
            .append_native_event(&AgentEvent::new(
                &session.id,
                6,
                AgentEventKind::CheckpointCommitted {
                    reference: reference.clone(),
                },
            ))
            .unwrap();
        app.view.selected_session = Some(session.id);
        app.view.workspace = root.display().to_string();
        (app, reference)
    }

    #[test]
    fn checkpoint_model_transition_is_local_durable_and_preserves_prior_data() {
        for source_format in [
            "responses-v1",
            "chat-completions-v1",
            "anthropic-messages-v1",
        ] {
            let root = tempfile::tempdir().unwrap();
            let (app, original_reference) = checkpoint_fixture(root.path(), source_format);
            let store = app.store.as_ref().unwrap();
            let owner = app.view.selected_session.as_deref().unwrap();
            let checkpoints = CheckpointFileStore::new(app.data_root.join("sessions")).unwrap();
            let original = checkpoints
                .load_after_commit(Some(&original_reference))
                .unwrap()
                .unwrap();
            let before = store.load(owner).unwrap().unwrap();
            let lease = store.try_acquire_native_lease(owner).unwrap();
            let target = native_checkpoint_compatibility(
                "new-provider",
                "new-model",
                NativeWireApi::Responses,
            );
            let marker = migrate_checkpoint_for_model(&app.data_root, store, &lease, &target)
                .unwrap()
                .unwrap();
            assert_eq!(marker.sequence, 7);
            let after = store.load(owner).unwrap().unwrap();
            assert_eq!(
                &after.events[..before.events.len()],
                before.events.as_slice(),
                "the original journal is immutable"
            );
            let replay = replay_stored_native(&app.data_root, &after).unwrap();
            assert!(replay.can_start_new_turn);
            let migrated = replay.checkpoint.unwrap();
            assert_ne!(migrated.checkpoint_id, original.checkpoint_id);
            assert_eq!(migrated.compatibility, target);
            assert_eq!(migrated.summary, original.summary);
            assert_eq!(
                migrated.retained_user_messages,
                original.retained_user_messages
            );
            assert_eq!(
                migrated.covered_event_sequence,
                original.covered_event_sequence
            );
            assert!(migrated.budget.limits_unverified);
            assert_eq!(
                checkpoints
                    .load_after_commit(Some(&original_reference))
                    .unwrap()
                    .unwrap(),
                original
            );
            assert!(
                migrate_checkpoint_for_model(&app.data_root, store, &lease, &target)
                    .unwrap()
                    .is_none(),
                "idempotent route selection adds no extra marker"
            );
        }
    }

    #[test]
    fn unknown_checkpoint_format_is_rejected_without_new_marker_or_mutating_the_original() {
        let root = tempfile::tempdir().unwrap();
        let (app, reference) = checkpoint_fixture(root.path(), "future-summary-v9");
        let owner = app.view.selected_session.as_deref().unwrap();
        let store = app.store.as_ref().unwrap();
        let before = store.load(owner).unwrap().unwrap();
        let lease = store.try_acquire_native_lease(owner).unwrap();
        let target = native_checkpoint_compatibility("provider", "model", NativeWireApi::Responses);
        let error =
            migrate_checkpoint_for_model(&app.data_root, store, &lease, &target).unwrap_err();
        assert!(error.contains("crea una nueva tarea"));
        assert_eq!(store.load(owner).unwrap().unwrap().events, before.events);
        let checkpoints = CheckpointFileStore::new(app.data_root.join("sessions")).unwrap();
        assert_eq!(
            checkpoints
                .load_after_commit(Some(&reference))
                .unwrap()
                .unwrap()
                .compatibility
                .context_format_key,
            "future-summary-v9"
        );
    }

    #[test]
    fn queued_model_keeps_an_actionable_pending_error_when_checkpoint_cannot_migrate() {
        let root = tempfile::tempdir().unwrap();
        let (mut app, _) = checkpoint_fixture(root.path(), "future-summary-v9");
        let old_model = app.view.model.clone();
        let owner = app.view.selected_session.clone().unwrap();
        let before = app
            .store
            .as_ref()
            .unwrap()
            .load(&owner)
            .unwrap()
            .unwrap()
            .events;
        let route = app
            .view
            .composer_routes
            .iter()
            .find(|route| route.model_id == "gpt-5.6-luna")
            .unwrap()
            .route_id
            .clone();
        app.view.running = true;
        assert!(app.select_composer_route(&route));
        app.view.running = false;
        app.poll_pending_composer_selection();
        let pending = app.view.composer_pending.as_ref().unwrap();
        assert!(pending
            .error
            .as_ref()
            .unwrap()
            .contains("crea una nueva tarea"));
        let token = pending.token;
        assert_eq!(app.view.model, old_model);
        assert_eq!(
            app.store
                .as_ref()
                .unwrap()
                .load(&owner)
                .unwrap()
                .unwrap()
                .events,
            before
        );
        app.undo_pending_composer_selection(token);
        assert!(app.view.composer_pending.is_none());
        assert!(app.view.composer_error.is_none());
    }

    #[test]
    fn failed_model_preferences_after_checkpoint_publish_can_undo_and_return_to_previous_model() {
        let root = tempfile::tempdir().unwrap();
        let (mut app, original_reference) = checkpoint_fixture(root.path(), "responses-v1");
        let old_provider = app.view.native_provider_id.clone();
        let old_model = app.view.model.clone();
        let owner = app.view.selected_session.clone().unwrap();
        let store = app.store.clone().unwrap();
        let checkpoints = CheckpointFileStore::new(app.data_root.join("sessions")).unwrap();
        let original = checkpoints
            .load_after_commit(Some(&original_reference))
            .unwrap()
            .unwrap();
        let before = store.load(&owner).unwrap().unwrap().events;
        let route = app
            .view
            .composer_routes
            .iter()
            .find(|route| route.model_id == "gpt-5.6-luna")
            .unwrap()
            .route_id
            .clone();
        app.view.running = true;
        assert!(app.select_composer_route(&route));
        let preferences_path = app.data_root.join("native-provider.json");
        let saved_preferences = std::fs::read(&preferences_path).unwrap();
        std::fs::remove_file(&preferences_path).unwrap();
        std::fs::create_dir(&preferences_path).unwrap();
        app.view.running = false;
        app.poll_pending_composer_selection();
        let pending = app.view.composer_pending.as_ref().unwrap();
        assert!(pending.error.is_some());
        let token = pending.token;
        assert_eq!(app.view.model, old_model);
        let migrated =
            replay_stored_native(&app.data_root, &store.load(&owner).unwrap().unwrap()).unwrap();
        assert_eq!(
            migrated.checkpoint.as_ref().unwrap().compatibility.model,
            "gpt-5.6-luna"
        );
        app.undo_pending_composer_selection(token);
        assert!(app.view.composer_pending.is_none());
        std::fs::remove_dir(&preferences_path).unwrap();
        std::fs::write(&preferences_path, saved_preferences).unwrap();
        let adapter =
            native_provider::adapter_for_model_at(&app.data_root, &old_provider, &old_model)
                .unwrap()
                .unwrap();
        let target = native_checkpoint_compatibility(&old_provider, &old_model, adapter.wire_api);
        let lease = store.try_acquire_native_lease(&owner).unwrap();
        assert!(
            migrate_checkpoint_for_model(&app.data_root, &store, &lease, &target)
                .unwrap()
                .is_some()
        );
        let after = store.load(&owner).unwrap().unwrap();
        assert_eq!(&after.events[..before.len()], before.as_slice());
        let replay = replay_stored_native(&app.data_root, &after).unwrap();
        assert!(replay.can_start_new_turn);
        let returned = replay.checkpoint.unwrap();
        assert_eq!(returned.compatibility, target);
        assert_eq!(returned.summary, original.summary);
        assert_eq!(
            returned.retained_user_messages,
            original.retained_user_messages
        );
        assert_eq!(
            returned.covered_event_sequence,
            original.covered_event_sequence
        );
        assert_eq!(
            checkpoints
                .load_after_commit(Some(&original_reference))
                .unwrap()
                .unwrap(),
            original
        );
    }
}
