//! A loop closed through a wireless link fails `topo_sort` as an edge loop
//! does, so the editor's link commands can refuse it (#1135): a looping
//! grade can't be saved in revision 2.

use klipp_nodes::NodeGraph;

#[test]
fn a_wireless_link_that_closes_a_loop_fails_topo_sort() {
    let mut g = NodeGraph::default();
    assert!(g.nodes.len() >= 2, "the default graph has a chain to loop");
    assert!(g.topo_sort().is_ok());
    let (first, last) = (g.nodes.first().unwrap().id, g.nodes.last().unwrap().id);
    // The last node sends on "x", the first receives "x": a loop with no edge.
    g.nodes.iter_mut().find(|n| n.id == last).unwrap().output_link = Some("x".into());
    g.nodes.iter_mut().find(|n| n.id == first).unwrap().input_links = vec![Some("x".into())];
    assert!(g.topo_sort().is_err(), "a loop through a wireless link");
    // The link alone, on a chain that doesn't come back, is fine.
    g.nodes.iter_mut().find(|n| n.id == first).unwrap().input_links.clear();
    assert!(g.topo_sort().is_ok());
}
