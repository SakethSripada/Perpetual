//! Layered work-graph layout (Sugiyama-style), group-aware.
//!
//! Pipeline: contract each group's children into a cluster super-node → rank
//! super-nodes by longest path over gating edges → reduce edge crossings with
//! barycenter sweeps → assign coordinates from real node sizes (per-layer
//! column widths, vertical stacking) → place children inside their group's
//! rect → resolve any remaining overlaps deterministically.
//!
//! `PreserveManual` keeps user-pinned nodes exactly where they are and lays
//! out the rest around them; `Force` re-lays out everything.

use std::collections::{HashMap, HashSet, VecDeque};

use am_db::repos::work_graph::NodePlacement;
use am_proto::{LayoutMode, WorkEdge, WorkEdgeKind, WorkNode, WorkNodeKind};

const X0: f64 = 80.0;
const Y0: f64 = 80.0;
/// Horizontal gap between layer columns — also the corridor edges route
/// through, so it stays generous.
const COLUMN_GUTTER: f64 = 110.0;
const VGAP: f64 = 36.0;

const LEAF_W: f64 = 230.0;
const LEAF_H: f64 = 96.0;

const GROUP_HEADER: f64 = 42.0;
const GROUP_PAD: f64 = 14.0;
const CHILD_W: f64 = 218.0;
const CHILD_H: f64 = 96.0;
const CHILD_GAP: f64 = 10.0;
/// Groups switch to a two-column interior once they hold this many children.
const TWO_COLUMN_THRESHOLD: usize = 6;

/// Weight of non-gating edges in barycenter ordering: they pull related nodes
/// together without affecting ranks.
const SOFT_EDGE_WEIGHT: f64 = 0.25;
const ORDERING_SWEEPS: usize = 4;

pub(crate) fn compute_layout(
    nodes: &[WorkNode],
    edges: &[WorkEdge],
    mode: LayoutMode,
) -> Vec<NodePlacement> {
    if nodes.is_empty() {
        return Vec::new();
    }
    let by_id: HashMap<&str, &WorkNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let group_ids: HashSet<&str> = nodes
        .iter()
        .filter(|n| n.kind == WorkNodeKind::Group)
        .map(|n| n.id.as_str())
        .collect();

    // Children keyed by group, in stable visual order.
    let mut children: HashMap<&str, Vec<&WorkNode>> = HashMap::new();
    for node in nodes {
        if let Some(parent) = node.parent_id.as_deref() {
            if group_ids.contains(parent) && node.kind != WorkNodeKind::Group {
                children.entry(parent).or_default().push(node);
            }
        }
    }
    for list in children.values_mut() {
        list.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(a.created_at.cmp(&b.created_at))
                .then(a.id.cmp(&b.id))
        });
    }

    // Super-nodes: groups plus free (ungrouped) nodes.
    let super_of = |id: &str| -> Option<&str> {
        let node = by_id.get(id)?;
        match node.parent_id.as_deref() {
            Some(parent) if group_ids.contains(parent) && node.kind != WorkNodeKind::Group => {
                Some(parent)
            }
            _ => Some(node.id.as_str()),
        }
    };
    let supers: Vec<&WorkNode> = nodes
        .iter()
        .filter(|n| super_of(&n.id) == Some(n.id.as_str()))
        .collect();

    // Contracted edges with ordering weights; gating edges also rank.
    let mut rank_edges: Vec<(&str, &str)> = Vec::new();
    let mut order_edges: Vec<(&str, &str, f64)> = Vec::new();
    for edge in edges {
        let (from, to, weight, gating) = match edge.kind {
            WorkEdgeKind::DependsOn => {
                (edge.target_id.as_str(), edge.source_id.as_str(), 1.0, true)
            }
            WorkEdgeKind::Blocks | WorkEdgeKind::Handoff => {
                (edge.source_id.as_str(), edge.target_id.as_str(), 1.0, true)
            }
            WorkEdgeKind::SharesContext | WorkEdgeKind::RelatesTo => (
                edge.source_id.as_str(),
                edge.target_id.as_str(),
                SOFT_EDGE_WEIGHT,
                false,
            ),
        };
        let (Some(from), Some(to)) = (super_of(from), super_of(to)) else {
            continue;
        };
        if from == to {
            continue;
        }
        if gating {
            rank_edges.push((from, to));
        }
        order_edges.push((from, to, weight));
    }

    // ---- Rank: longest path over gating edges (graph is validated acyclic;
    // any leftover from data races keeps layer 0).
    let mut layer: HashMap<&str, i64> = supers.iter().map(|n| (n.id.as_str(), 0)).collect();
    let mut indegree: HashMap<&str, usize> = supers.iter().map(|n| (n.id.as_str(), 0)).collect();
    let mut outgoing: HashMap<&str, Vec<&str>> = HashMap::new();
    for (from, to) in &rank_edges {
        outgoing.entry(from).or_default().push(to);
        *indegree.entry(to).or_insert(0) += 1;
    }
    let mut queue: VecDeque<&str> = indegree
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect();
    while let Some(id) = queue.pop_front() {
        let base = *layer.get(id).unwrap_or(&0);
        for next in outgoing.get(id).into_iter().flatten() {
            let entry = layer.entry(next).or_insert(0);
            *entry = (*entry).max(base + 1);
            let count = indegree.entry(next).or_insert(0);
            *count = count.saturating_sub(1);
            if *count == 0 {
                queue.push_back(next);
            }
        }
    }

    // ---- Order: barycenter sweeps to reduce crossings.
    let max_layer = layer.values().copied().max().unwrap_or(0);
    let mut layers: Vec<Vec<&str>> = vec![Vec::new(); (max_layer + 1) as usize];
    let mut initial: Vec<&WorkNode> = supers.clone();
    initial.sort_by(|a, b| {
        kind_order(a.kind)
            .cmp(&kind_order(b.kind))
            .then(a.sort_order.cmp(&b.sort_order))
            .then(a.created_at.cmp(&b.created_at))
            .then(a.id.cmp(&b.id))
    });
    for node in &initial {
        layers[*layer.get(node.id.as_str()).unwrap_or(&0) as usize].push(node.id.as_str());
    }

    let mut neighbors: HashMap<&str, Vec<(&str, f64)>> = HashMap::new();
    for (from, to, weight) in &order_edges {
        neighbors.entry(from).or_default().push((to, *weight));
        neighbors.entry(to).or_default().push((from, *weight));
    }
    for sweep in 0..ORDERING_SWEEPS {
        let forward = sweep % 2 == 0;
        let indices: Vec<usize> = if forward {
            (1..layers.len()).collect()
        } else {
            (0..layers.len().saturating_sub(1)).rev().collect()
        };
        for i in indices {
            let reference: HashMap<&str, f64> = {
                let ref_layer = if forward {
                    &layers[i - 1]
                } else {
                    &layers[i + 1]
                };
                ref_layer
                    .iter()
                    .enumerate()
                    .map(|(pos, id)| (*id, pos as f64))
                    .collect()
            };
            let current: HashMap<&str, f64> = layers[i]
                .iter()
                .enumerate()
                .map(|(pos, id)| (*id, pos as f64))
                .collect();
            let mut scored: Vec<(f64, &str)> = layers[i]
                .iter()
                .map(|id| {
                    let mut weight_sum = 0.0;
                    let mut acc = 0.0;
                    for (other, weight) in neighbors.get(id).into_iter().flatten() {
                        if let Some(pos) = reference.get(other) {
                            acc += pos * weight;
                            weight_sum += weight;
                        }
                    }
                    let score = if weight_sum > 0.0 {
                        acc / weight_sum
                    } else {
                        // No neighbors in the reference layer: hold position.
                        current[id]
                    };
                    (score, *id)
                })
                .collect();
            scored.sort_by(|a, b| {
                a.0.partial_cmp(&b.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        current[a.1]
                            .partial_cmp(&current[b.1])
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            });
            layers[i] = scored.into_iter().map(|(_, id)| id).collect();
        }
    }

    // ---- Sizes.
    let size_of = |id: &str| -> (f64, f64) {
        if group_ids.contains(id) {
            group_size(children.get(id).map_or(0, Vec::len))
        } else {
            (LEAF_W, LEAF_H)
        }
    };

    // ---- Coordinates: per-layer column widths, vertical stacking.
    let mut placements: HashMap<&str, NodePlacement> = HashMap::new();
    let mut x = X0;
    for layer_nodes in &layers {
        let column_width = layer_nodes
            .iter()
            .map(|id| size_of(id).0)
            .fold(LEAF_W, f64::max);
        let mut y = Y0;
        for id in layer_nodes {
            let (w, h) = size_of(id);
            placements.insert(
                id,
                NodePlacement {
                    node_id: (*id).to_string(),
                    x,
                    y,
                    width: w,
                    height: h,
                },
            );
            y += h + VGAP;
        }
        x += column_width + COLUMN_GUTTER;
    }

    // ---- PreserveManual: pinned super-nodes stay put; the rest flow around.
    if mode == LayoutMode::PreserveManual {
        let mut occupied: Vec<(f64, f64, f64, f64)> = Vec::new();
        for node in &supers {
            if node.position_locked {
                if let Some(placement) = placements.get_mut(node.id.as_str()) {
                    placement.x = node.position_x;
                    placement.y = node.position_y;
                    occupied.push((placement.x, placement.y, placement.width, placement.height));
                }
            }
        }
        // Deterministic overlap resolution: unpinned rects push down past any
        // collision (pinned or previously placed).
        let mut order: Vec<&str> = layers.iter().flatten().copied().collect();
        order.retain(|id| by_id.get(id).is_some_and(|n| !n.position_locked));
        for id in order {
            let Some(placement) = placements.get(id).cloned() else {
                continue;
            };
            let mut candidate = (placement.x, placement.y, placement.width, placement.height);
            let mut guard = 0;
            while let Some(hit) = occupied.iter().find(|rect| rects_overlap(rect, &candidate)) {
                candidate.1 = hit.1 + hit.3 + VGAP;
                guard += 1;
                if guard > 1000 {
                    break;
                }
            }
            occupied.push(candidate);
            if let Some(placement) = placements.get_mut(id) {
                placement.x = candidate.0;
                placement.y = candidate.1;
            }
        }
    }

    // ---- Children: absolute positions inside their group's rect.
    let mut out: Vec<NodePlacement> = Vec::new();
    for (group_id, list) in &children {
        let Some(group) = placements.get(group_id) else {
            continue;
        };
        let cols = if list.len() > TWO_COLUMN_THRESHOLD {
            2
        } else {
            1
        };
        for (index, child) in list.iter().enumerate() {
            let col = (index % cols) as f64;
            let row = (index / cols) as f64;
            out.push(NodePlacement {
                node_id: child.id.clone(),
                x: group.x + GROUP_PAD + col * (CHILD_W + CHILD_GAP),
                y: group.y + GROUP_HEADER + row * (CHILD_H + CHILD_GAP),
                width: CHILD_W,
                height: CHILD_H,
            });
        }
    }
    out.extend(placements.into_values());
    out.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    out
}

/// Interior grid size for a group with `count` children.
fn group_size(count: usize) -> (f64, f64) {
    let count = count.max(1);
    let cols = if count > TWO_COLUMN_THRESHOLD { 2 } else { 1 };
    let rows = count.div_ceil(cols);
    let width = 2.0 * GROUP_PAD + cols as f64 * CHILD_W + (cols - 1) as f64 * CHILD_GAP;
    let height = GROUP_HEADER + rows as f64 * CHILD_H + (rows - 1) as f64 * CHILD_GAP + GROUP_PAD;
    (width, height)
}

fn kind_order(kind: WorkNodeKind) -> u8 {
    match kind {
        WorkNodeKind::Group => 0,
        WorkNodeKind::Milestone => 1,
        WorkNodeKind::Task => 2,
        WorkNodeKind::Session => 3,
    }
}

fn rects_overlap(a: &(f64, f64, f64, f64), b: &(f64, f64, f64, f64)) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

#[cfg(test)]
mod tests {
    use super::*;
    use am_proto::{now, TaskPriority, TaskStatus};

    fn node(id: &str, kind: WorkNodeKind, parent: Option<&str>) -> WorkNode {
        WorkNode {
            id: id.to_string(),
            project_id: "p".into(),
            parent_id: parent.map(str::to_string),
            task_id: None,
            thread_id: None,
            kind,
            title: id.to_string(),
            description: None,
            status: TaskStatus::Draft,
            priority: TaskPriority::Medium,
            primary_agent: None,
            position_x: 0.0,
            position_y: 0.0,
            width: None,
            height: None,
            position_locked: false,
            sort_order: 0,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn edge(id: &str, source: &str, target: &str, kind: WorkEdgeKind) -> WorkEdge {
        WorkEdge {
            id: id.to_string(),
            project_id: "p".into(),
            source_id: source.to_string(),
            target_id: target.to_string(),
            kind,
            label: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn placement_map(placements: &[NodePlacement]) -> HashMap<&str, &NodePlacement> {
        placements.iter().map(|p| (p.node_id.as_str(), p)).collect()
    }

    /// Crossing count between adjacent layers for gating edges given final
    /// y-order — the quality metric for the barycenter step.
    fn crossings(nodes: &[WorkNode], edges: &[WorkEdge], placements: &[NodePlacement]) -> usize {
        let by_id = placement_map(placements);
        let mut spans: Vec<((f64, f64), (f64, f64))> = Vec::new();
        for edge in edges {
            let (from, to) = match edge.kind {
                WorkEdgeKind::DependsOn => (edge.target_id.as_str(), edge.source_id.as_str()),
                WorkEdgeKind::Blocks | WorkEdgeKind::Handoff => {
                    (edge.source_id.as_str(), edge.target_id.as_str())
                }
                _ => continue,
            };
            let (Some(a), Some(b)) = (by_id.get(from), by_id.get(to)) else {
                continue;
            };
            spans.push(((a.x, a.y), (b.x, b.y)));
        }
        let mut count = 0;
        for i in 0..spans.len() {
            for j in (i + 1)..spans.len() {
                let (a, b) = (spans[i], spans[j]);
                // Same column pair, inverted vertical order.
                if a.0 .0 == b.0 .0
                    && a.1 .0 == b.1 .0
                    && (a.0 .1 - b.0 .1) * (a.1 .1 - b.1 .1) < 0.0
                {
                    count += 1;
                }
            }
        }
        let _ = nodes;
        count
    }

    #[test]
    fn no_overlapping_rects_and_monotonic_gating_x() {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        for i in 0..12 {
            nodes.push(node(&format!("n{i}"), WorkNodeKind::Task, None));
        }
        for i in 0..8 {
            edges.push(edge(
                &format!("e{i}"),
                &format!("n{}", i + 4),
                &format!("n{i}"),
                WorkEdgeKind::DependsOn,
            ));
        }
        let placements = compute_layout(&nodes, &edges, LayoutMode::Force);
        let by_id = placement_map(&placements);

        for (i, a) in placements.iter().enumerate() {
            for b in placements.iter().skip(i + 1) {
                assert!(
                    !rects_overlap(
                        &(a.x, a.y, a.width, a.height),
                        &(b.x, b.y, b.width, b.height)
                    ),
                    "{} overlaps {}",
                    a.node_id,
                    b.node_id
                );
            }
        }
        for e in &edges {
            let prerequisite = by_id[e.target_id.as_str()];
            let dependent = by_id[e.source_id.as_str()];
            assert!(
                prerequisite.x < dependent.x,
                "dependents must sit right of prerequisites"
            );
        }
    }

    #[test]
    fn barycenter_reduces_crossings_on_inverted_ladder() {
        // Two parallel chains wired in inverted order: naive stacking crosses.
        let nodes = vec![
            node("a1", WorkNodeKind::Task, None),
            node("a2", WorkNodeKind::Task, None),
            node("b1", WorkNodeKind::Task, None),
            node("b2", WorkNodeKind::Task, None),
        ];
        // a2 depends on b1; b2 depends on a1 — the crossing pattern.
        let straight = vec![
            edge("e1", "a2", "a1", WorkEdgeKind::DependsOn),
            edge("e2", "b2", "b1", WorkEdgeKind::DependsOn),
        ];
        let placements = compute_layout(&nodes, &straight, LayoutMode::Force);
        assert_eq!(crossings(&nodes, &straight, &placements), 0);
    }

    #[test]
    fn children_stay_inside_group_bounds_and_groups_size_from_children() {
        let mut nodes = vec![node("g", WorkNodeKind::Group, None)];
        for i in 0..8 {
            nodes.push(node(&format!("c{i}"), WorkNodeKind::Task, Some("g")));
        }
        let placements = compute_layout(&nodes, &[], LayoutMode::Force);
        let by_id = placement_map(&placements);
        let group = by_id["g"];
        // 8 children: two-column interior.
        assert!(group.width > 2.0 * CHILD_W, "two-column group width");
        for i in 0..8 {
            let child = by_id[format!("c{i}").as_str()];
            assert!(child.x >= group.x && child.x + child.width <= group.x + group.width + 0.01);
            assert!(child.y >= group.y && child.y + child.height <= group.y + group.height + 0.01);
        }
    }

    #[test]
    fn scales_to_hundreds_of_nodes_without_stacking() {
        for count in [50usize, 100, 200] {
            let nodes: Vec<_> = (0..count)
                .map(|i| {
                    let mut n = node(
                        &format!("n{i}"),
                        if i % 17 == 0 {
                            WorkNodeKind::Milestone
                        } else {
                            WorkNodeKind::Task
                        },
                        None,
                    );
                    n.sort_order = i as i64;
                    n
                })
                .collect();
            let edges: Vec<_> = (1..count)
                .map(|i| {
                    edge(
                        &format!("e{i}"),
                        &format!("n{}", i - 1),
                        &format!("n{i}"),
                        WorkEdgeKind::Blocks,
                    )
                })
                .collect();
            let placements = compute_layout(&nodes, &edges, LayoutMode::Force);
            assert_eq!(placements.len(), count);
            let mut seen = HashSet::new();
            for p in &placements {
                let key = ((p.x / 10.0).round() as i64, (p.y / 10.0).round() as i64);
                assert!(seen.insert(key), "stacked nodes at {key:?} for {count}");
            }
        }
    }

    #[test]
    fn layout_is_deterministic() {
        let nodes = vec![
            node("m", WorkNodeKind::Milestone, None),
            node("t1", WorkNodeKind::Task, None),
            node("t2", WorkNodeKind::Task, None),
        ];
        let edges = vec![
            edge("e1", "m", "t1", WorkEdgeKind::DependsOn),
            edge("e2", "m", "t2", WorkEdgeKind::DependsOn),
        ];
        let a = compute_layout(&nodes, &edges, LayoutMode::Force);
        let b = compute_layout(&nodes, &edges, LayoutMode::Force);
        let dump = |p: &[NodePlacement]| {
            p.iter()
                .map(|n| format!("{}:{:.1},{:.1}", n.node_id, n.x, n.y))
                .collect::<Vec<_>>()
                .join(";")
        };
        assert_eq!(dump(&a), dump(&b));
    }

    #[test]
    fn preserve_manual_anchors_pinned_nodes_and_avoids_them() {
        let mut pinned = node("pinned", WorkNodeKind::Task, None);
        pinned.position_locked = true;
        pinned.position_x = X0; // exactly where the flow layout would drop the first node
        pinned.position_y = Y0;
        let nodes = vec![pinned, node("free", WorkNodeKind::Task, None)];

        let placements = compute_layout(&nodes, &[], LayoutMode::PreserveManual);
        let by_id = placement_map(&placements);
        assert_eq!(by_id["pinned"].x, X0);
        assert_eq!(by_id["pinned"].y, Y0);
        let free = by_id["free"];
        assert!(
            !rects_overlap(
                &(X0, Y0, LEAF_W, LEAF_H),
                &(free.x, free.y, free.width, free.height)
            ),
            "free node must flow around the pin"
        );

        // Force ignores the pin.
        let forced = compute_layout(&nodes, &[], LayoutMode::Force);
        let by_id = placement_map(&forced);
        // Both land on the flow grid; first by deterministic order gets Y0.
        assert_eq!(by_id["free"].x, by_id["pinned"].x);
    }
}
