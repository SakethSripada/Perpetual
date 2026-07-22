-- Claude subscription percentage telemetry is not a dependable host contract.
-- Keep existing sessions usable by converting the experimental mode to the
-- safe default; users can choose a token target instead.
UPDATE agent_threads
SET task_budget = '{"mode":"unlimited"}'
WHERE task_budget LIKE '%"mode":"claude_percent"%';
