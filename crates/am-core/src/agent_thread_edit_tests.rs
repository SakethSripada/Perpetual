use super::*;
async fn fixture() -> (AppCore, AgentThread, Vec<AgentThreadEvent>) {
    let core = crate::test_core().await;
    let thread = core
        .create_agent_thread(NewAgentThread {
            title: "Edit test".into(),
            objective: Some("First request".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut events = vec![];
    for text in ["First request", "Old followup"] {
        let turn = am_db::repos::agent_turn::create(
            &core.db.pool,
            &thread.id,
            AgentKind::Codex,
            "read_only",
            ExecutionBackend::Host,
            None,
            None,
            None,
            None,
            None,
            ModelTargetKind::default(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        sqlx::query(
            "UPDATE agent_turns SET agent_session_id = 'stale-provider-session' WHERE id = ?",
        )
        .bind(&turn.id)
        .execute(&core.db.pool)
        .await
        .unwrap();
        let user = user_thread_event(&thread.id, &turn.id, text, None);
        am_db::repos::agent_thread_message::insert(&core.db.pool, &user)
            .await
            .unwrap();
        let mut reply = user.clone();
        reply.id = new_id();
        reply.role = "assistant".into();
        reply.kind = "assistant_text".into();
        reply.text = Some(format!("Reply to {text}"));
        am_db::repos::agent_thread_message::insert(&core.db.pool, &reply)
            .await
            .unwrap();
        events.extend([user, reply]);
    }
    (core, thread, events)
}
#[tokio::test]
async fn editing_rewinds_all_providers_and_retains_only_earlier_context() {
    for agent in [AgentKind::Codex, AgentKind::ClaudeCode] {
        let (core, thread, events) = fixture().await;
        let mut policy = core.get_limit_policy().await.unwrap();
        policy.auto_switch = false;
        policy.dismissed_system_accounts = vec![AgentKind::Codex, AgentKind::ClaudeCode];
        core.set_limit_policy(policy).await.unwrap();
        core.record_agent_probe(am_agents::AgentInstallStatus {
            kind: agent,
            installed: true,
            authenticated: true,
            version: None,
            binary_path: None,
        })
        .await
        .unwrap();
        core.mark_agent_limited(agent, Some(now() + chrono::Duration::hours(1)))
            .await
            .unwrap();
        core.edit_thread_message(
            &thread.id,
            &events[2].id,
            agent,
            PermissionPolicy::ReadOnly,
            "Replacement followup".into(),
            Some("edited-id".into()),
        )
        .await
        .unwrap();
        let retained = core.list_thread_events(&thread.id).await.unwrap();

        assert!(!retained
            .iter()
            .any(|e| e.id == events[2].id || e.id == events[3].id));
        assert_eq!(retained[0].id, events[0].id);
        assert_eq!(retained[1].id, events[1].id);
        assert!(core
            .list_thread_turns(&thread.id)
            .await
            .unwrap()
            .iter()
            .all(|t| t.agent_session_id.is_none()));
        let saved = core.get_agent_thread(&thread.id).await.unwrap().unwrap();
        assert!(saved.progress.contains("First request"));
        assert!(!saved.progress.contains("Old followup"));
        let queued = core.list_queued_turns(&thread.id).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].message, "Replacement followup");
        assert_eq!(queued[0].client_message_id.as_deref(), Some("edited-id"));
    }
}
#[tokio::test]
async fn failed_edit_preflight_restores_the_conversation_and_provider_sessions() {
    let (core, thread, events) = fixture().await;
    let error = core
        .edit_thread_message(
            &thread.id,
            &events[2].id,
            AgentKind::Gemini,
            PermissionPolicy::ReadOnly,
            "Replacement".into(),
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no adapter available"));
    let restored = core.list_thread_events(&thread.id).await.unwrap();
    assert_eq!(
        restored.iter().map(|e| &e.id).collect::<Vec<_>>(),
        events.iter().map(|e| &e.id).collect::<Vec<_>>()
    );
    assert!(core
        .list_thread_turns(&thread.id)
        .await
        .unwrap()
        .iter()
        .all(|t| t.agent_session_id.as_deref() == Some("stale-provider-session")));
    assert_eq!(
        core.get_agent_thread(&thread.id)
            .await
            .unwrap()
            .unwrap()
            .objective,
        "First request"
    );
}
#[tokio::test]
async fn edit_rejects_stale_targets_and_pending_work_without_mutating_history() {
    let (core, thread, events) = fixture().await;
    assert!(core
        .edit_thread_message(
            &thread.id,
            "missing-id",
            AgentKind::Codex,
            PermissionPolicy::ReadOnly,
            "Replacement".into(),
            None
        )
        .await
        .is_err());
    am_db::repos::queued_turn::enqueue_with_echo(
        &core.db.pool,
        &thread.id,
        AgentKind::Codex,
        "read_only",
        "Pending",
        None,
        true,
        None,
    )
    .await
    .unwrap();
    assert!(core
        .edit_thread_message(
            &thread.id,
            &events[0].id,
            AgentKind::ClaudeCode,
            PermissionPolicy::ReadOnly,
            "Replacement".into(),
            None
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("queued"));
    assert_eq!(core.list_thread_events(&thread.id).await.unwrap().len(), 4);
}
