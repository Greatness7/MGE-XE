use std::collections::VecDeque;

use anyhow::Context;
use itertools::Itertools;

#[derive(Clone)]
struct FlowEdge {
    to: usize,
    reverse: usize,
    capacity: u8,
    cost: i128,
}

fn add_flow_edge(graph: &mut [Vec<FlowEdge>], from: usize, to: usize, capacity: u8, cost: i128) -> usize {
    let forward = graph[from].len();
    let reverse = graph[to].len();
    graph[from].push(FlowEdge {
        to,
        reverse,
        capacity,
        cost,
    });
    graph[to].push(FlowEdge {
        to: from,
        reverse: forward,
        capacity: 0,
        cost: -cost,
    });
    forward
}

/// Maximum-weight bipartite matching over non-negative edge weights, solved as a min-cost
/// max-flow with successive shortest augmenting paths (SPFA relaxation).
///
/// Weights are encoded as negative costs scaled above any tie-break contribution, so augmentation
/// continues while paths remain profitable and stops at the first non-negative shortest path.
pub(super) fn min_cost_positive_matching(
    group_count: usize,
    slot_count: usize,
    edges: &[(usize, usize, usize)],
) -> anyhow::Result<Vec<(usize, usize)>> {
    let source = 0;
    let group_base = 1;
    let slot_base = group_base + group_count;
    let sink = slot_base + slot_count;
    let mut graph = vec![Vec::new(); sink + 1];
    for group in 0..group_count {
        add_flow_edge(&mut graph, source, group_base + group, 1, 0);
    }
    for slot in 0..slot_count {
        add_flow_edge(&mut graph, slot_base + slot, sink, 1, 0);
    }
    let max_pairs = group_count.min(slot_count).max(1) as i128;
    let max_tie = slot_count
        .saturating_mul(group_count.saturating_add(1))
        .saturating_add(group_count) as i128;
    let scale = max_tie
        .checked_add(1)
        .and_then(|value| value.checked_mul(max_pairs))
        .and_then(|value| value.checked_add(1))
        .context("atlas matching score overflow")?;
    let mut edge_positions = Vec::with_capacity(edges.len());
    let mut sorted_edges = edges.to_vec();
    sorted_edges.sort_unstable_by_key(|&(group, slot, _)| (slot, group));
    for &(group, slot, weight) in &sorted_edges {
        let primary = (weight as i128)
            .checked_mul(scale)
            .context("atlas matching weight overflow")?;
        let tie = slot
            .checked_mul(group_count.saturating_add(1))
            .and_then(|value| value.checked_add(group))
            .context("atlas matching tie-break overflow")? as i128;
        let cost = -primary + tie;
        let position = add_flow_edge(&mut graph, group_base + group, slot_base + slot, 1, cost);
        edge_positions.push((group, slot, group_base + group, position));
    }

    let mut distance = vec![i128::MAX; graph.len()];
    let mut predecessor = vec![None::<(usize, usize)>; graph.len()];
    let mut queued = vec![false; graph.len()];
    let mut queue = VecDeque::with_capacity(graph.len());

    loop {
        distance.fill(i128::MAX);
        predecessor.fill(None);
        queued.fill(false);
        queue.clear();
        queue.push_back(source);
        distance[source] = 0;
        queued[source] = true;
        while let Some(node) = queue.pop_front() {
            queued[node] = false;
            for (edge_index, edge) in graph[node].iter().enumerate() {
                if edge.capacity == 0 || distance[node] == i128::MAX {
                    continue;
                }
                let next = distance[node]
                    .checked_add(edge.cost)
                    .context("atlas matching path cost overflow")?;
                if next < distance[edge.to] {
                    distance[edge.to] = next;
                    predecessor[edge.to] = Some((node, edge_index));
                    if !queued[edge.to] {
                        queue.push_back(edge.to);
                        queued[edge.to] = true;
                    }
                }
            }
        }
        if distance[sink] >= 0 || distance[sink] == i128::MAX {
            break;
        }
        let mut node = sink;
        while node != source {
            let (previous, edge_index) = predecessor[node].context("atlas matching path is incomplete")?;
            let reverse = graph[previous][edge_index].reverse;
            graph[previous][edge_index].capacity = 0;
            graph[node][reverse].capacity = 1;
            node = previous;
        }
    }

    Ok(edge_positions
        .into_iter()
        .filter_map(|(group, slot, node, edge)| (graph[node][edge].capacity == 0).then_some((group, slot)))
        .sorted_unstable()
        .collect_vec())
}
