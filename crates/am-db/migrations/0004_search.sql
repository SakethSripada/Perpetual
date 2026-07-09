-- M6: unified full-text search across tasks, knowledge docs, and memory notes.
-- A single FTS5 virtual table kept in sync by triggers on the source tables.
-- `kind`/`entity_id`/`project_id`/`task_id` are stored but UNINDEXED (filter +
-- navigation metadata); only `title`/`body` are tokenized.
CREATE VIRTUAL TABLE search_index USING fts5(
    title,
    body,
    kind UNINDEXED,
    entity_id UNINDEXED,
    project_id UNINDEXED,
    task_id UNINDEXED,
    tokenize = 'porter unicode61'
);

-- Seed from rows that predate this migration.
INSERT INTO search_index (title, body, kind, entity_id, project_id, task_id)
    SELECT title, COALESCE(description, ''), 'task', id, project_id, id FROM tasks;
INSERT INTO search_index (title, body, kind, entity_id, project_id, task_id)
    SELECT title, body, 'doc', id, project_id, NULL FROM knowledge_docs;
INSERT INTO search_index (title, body, kind, entity_id, project_id, task_id)
    SELECT '', body, 'memory', id, project_id, task_id FROM memory_notes;

-- Tasks.
CREATE TRIGGER trg_tasks_search_ai AFTER INSERT ON tasks BEGIN
    INSERT INTO search_index (title, body, kind, entity_id, project_id, task_id)
        VALUES (new.title, COALESCE(new.description, ''), 'task', new.id, new.project_id, new.id);
END;
CREATE TRIGGER trg_tasks_search_au AFTER UPDATE ON tasks BEGIN
    DELETE FROM search_index WHERE kind = 'task' AND entity_id = old.id;
    INSERT INTO search_index (title, body, kind, entity_id, project_id, task_id)
        VALUES (new.title, COALESCE(new.description, ''), 'task', new.id, new.project_id, new.id);
END;
CREATE TRIGGER trg_tasks_search_ad AFTER DELETE ON tasks BEGIN
    DELETE FROM search_index WHERE kind = 'task' AND entity_id = old.id;
END;

-- Knowledge docs.
CREATE TRIGGER trg_docs_search_ai AFTER INSERT ON knowledge_docs BEGIN
    INSERT INTO search_index (title, body, kind, entity_id, project_id, task_id)
        VALUES (new.title, new.body, 'doc', new.id, new.project_id, NULL);
END;
CREATE TRIGGER trg_docs_search_au AFTER UPDATE ON knowledge_docs BEGIN
    DELETE FROM search_index WHERE kind = 'doc' AND entity_id = old.id;
    INSERT INTO search_index (title, body, kind, entity_id, project_id, task_id)
        VALUES (new.title, new.body, 'doc', new.id, new.project_id, NULL);
END;
CREATE TRIGGER trg_docs_search_ad AFTER DELETE ON knowledge_docs BEGIN
    DELETE FROM search_index WHERE kind = 'doc' AND entity_id = old.id;
END;

-- Memory notes.
CREATE TRIGGER trg_memory_search_ai AFTER INSERT ON memory_notes BEGIN
    INSERT INTO search_index (title, body, kind, entity_id, project_id, task_id)
        VALUES ('', new.body, 'memory', new.id, new.project_id, new.task_id);
END;
CREATE TRIGGER trg_memory_search_au AFTER UPDATE ON memory_notes BEGIN
    DELETE FROM search_index WHERE kind = 'memory' AND entity_id = old.id;
    INSERT INTO search_index (title, body, kind, entity_id, project_id, task_id)
        VALUES ('', new.body, 'memory', new.id, new.project_id, new.task_id);
END;
CREATE TRIGGER trg_memory_search_ad AFTER DELETE ON memory_notes BEGIN
    DELETE FROM search_index WHERE kind = 'memory' AND entity_id = old.id;
END;
