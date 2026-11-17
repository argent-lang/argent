use super::*;

#[test]
fn static_template_usage_is_merged_at_typed_request_sites_and_stays_entry_local() {
    let body = r#"
        unrestricted(next.value);
        CounterState updated = { count: count + 1 };
        if (finish) { become next <- Finished(updated); }
        else { become next <- Counter(updated); }
    "#;
    for (case, clauses, extra, expected) in [
        ("current-only", "", "", HiddenParamUsage::CurrentOutputsOnly),
        ("optional-input", "consumes { peers: Finished[0..=1], }", "", HiddenParamUsage::General),
        ("observed-input", "observes remote by other_id { inputs { peer: Finished, } }", "", HiddenParamUsage::General),
        (
            "observed-output",
            "observes remote by other_id { inputs { worker: Worker, } outputs { result: Finished, } }",
            "require remote.outputs become { result <- Finished(CounterState { count: 9 }), };",
            HiddenParamUsage::General,
        ),
        (
            "spawn",
            "spawns children by children_id { outputs { child: Finished, } }",
            "unrestricted(children.outputs.child.value); require children.outputs become { child <- Finished(CounterState { count: 9 }), };",
            HiddenParamUsage::General,
        ),
    ] {
        let artifact = inline_artifact(
            case,
            &format!(
                r#"
            state CounterState {{ int count; }}
            actor Counter owns CounterState {{
                entry current(bool finish) emits next: Counter | Finished {{ {body} }}
                entry advance(bool finish, cov_id other_id) {clauses} emits next: Counter | Finished {{ {extra} {body} }}
            }}
            actor Finished owns CounterState {{ entry close() emits none {{ require(count > 0); }} }}
            actor Worker owns CounterState {{ entry close() emits none {{ require(count >= 0); }} }}
            app Test {{ actor Counter; actor Finished; actor Worker; }}
        "#
            ),
        );
        let counter = &artifact.argent.actors[0];
        let current = counter.entries.iter().find(|entry| entry.name == "current").expect("current entry");
        let advance = counter.entries.iter().find(|entry| entry.name == "advance").expect("advance entry");
        let target_params = advance
            .hidden_params
            .iter()
            .filter(|param| matches!(&param.subject, HiddenParamSubjectArtifact::Actor { actor } if actor == "Finished"))
            .collect::<Vec<_>>();
        assert_eq!(target_params.len(), 2, "{case}");
        for param in target_params {
            assert_eq!(param.usage, expected, "{case}: {}", param.name);
            if let Some(shared) = current.hidden_params.iter().find(|candidate| candidate.recipe_id == param.recipe_id) {
                assert_eq!(shared.usage, HiddenParamUsage::CurrentOutputsOnly, "recipe sharing must not merge entry use scopes");
            }
        }
        assert!(current.hidden_params.iter().all(|param| param.usage == HiddenParamUsage::CurrentOutputsOnly));
        if case == "optional-input" {
            assert!(
                advance
                    .hidden_params
                    .iter()
                    .all(|param| current.hidden_params.iter().any(|shared| shared.recipe_id == param.recipe_id))
            );
            assert!(advance.hidden_params.iter().all(|param| matches!(
                param.purpose,
                HiddenParamPurposeArtifact::TemplatePrefixBytes | HiddenParamPurposeArtifact::TemplateSuffixBytes
            )));
        }
    }
}

#[test]
fn selector_and_family_arguments_keep_general_usage() {
    let artifact = inline_artifact("general-selector-family-usage", include_str!("../../../../examples/route_state_bodies.ag"));
    let hidden =
        artifact.argent.actors.iter().flat_map(|actor| &actor.entries).flat_map(|entry| &entry.hidden_params).collect::<Vec<_>>();
    assert!(hidden.iter().any(|param| matches!(param.subject, HiddenParamSubjectArtifact::TemplateSelector { .. })));
    assert!(hidden.iter().any(|param| matches!(param.subject, HiddenParamSubjectArtifact::RouteFamily { .. })));
    for param in hidden {
        if !matches!(param.subject, HiddenParamSubjectArtifact::Actor { .. }) {
            assert_eq!(param.usage, HiddenParamUsage::General, "{}", param.name);
        }
    }
}
