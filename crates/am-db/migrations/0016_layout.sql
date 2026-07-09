-- Layout engine support: server-computed node sizes and explicit manual pins.

-- Groups get their size from the layout engine (children-driven); leaf nodes
-- carry their rendered footprint so layout and overlap checks use real rects.
ALTER TABLE work_nodes ADD COLUMN width REAL;
ALTER TABLE work_nodes ADD COLUMN height REAL;

-- Set when a user drags a node. PreserveManual layouts anchor these exactly;
-- Force layouts clear them. Replaces the fragile "nonzero position means
-- manually placed" heuristic.
ALTER TABLE work_nodes ADD COLUMN position_locked INTEGER NOT NULL DEFAULT 0;
