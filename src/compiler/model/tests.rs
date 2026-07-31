use super::*;

#[test]
fn compiler_route_leaves_preserve_packed_and_opened_commitment_nodes() {
    let mut graph = RouteGraph::default();
    graph.add_actor("Knight");
    graph.add_emit("Player", "Mux");
    graph.add_emit("Mux", "Knight");
    graph.add_emit("Mux", "Pawn");
    graph.add_emit("Pawn", "Mux");
    graph.add_emit("Mux", "Settle");
    let domains = BTreeMap::from([
        ("BoardState".to_string(), ["Knight", "Mux", "Pawn"].into_iter().map(str::to_string).collect()),
        ("PlayerState".to_string(), vec!["Player".to_string()]),
        ("SettleState".to_string(), vec!["Settle".to_string()]),
    ]);

    let plan = route_plan(&graph, &domains, &[]).expect("route plan is valid");
    let leaves = compiler_route_leaves(&plan).expect("commitment nodes lower to compiler leaves");

    assert_eq!(
        leaves["Player"],
        [
            RouteRootLeaf::Family("route_family/BoardState/mux".to_string()),
            RouteRootLeaf::Actor("Mux".to_string()),
            RouteRootLeaf::Actor("Settle".to_string()),
        ]
    );
    assert_eq!(
        leaves["Mux"],
        [
            RouteRootLeaf::Actor("Knight".to_string()),
            RouteRootLeaf::Actor("Pawn".to_string()),
            RouteRootLeaf::Actor("Mux".to_string()),
            RouteRootLeaf::Actor("Settle".to_string()),
        ]
    );
    assert!(leaves["Settle"].is_empty());

    let family_id = "route_family/BoardState/mux".to_string();
    assert_eq!(
        compiler_route_transition(&plan, "Player", "Mux").expect("Player can open the Mux family"),
        CompilerRouteTransition { families_to_open: vec![family_id.clone()], families_to_pack: Vec::new() }
    );
    assert_eq!(
        compiler_route_transition(&plan, "Mux", "Player").expect("Mux can pack its family for Player"),
        CompilerRouteTransition { families_to_open: Vec::new(), families_to_pack: vec![family_id] }
    );
}
