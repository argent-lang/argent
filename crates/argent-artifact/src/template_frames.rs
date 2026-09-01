//! Verification of app-local actor identity from compiled Sil template frames.

use std::fmt;

use thiserror::Error;

use crate::{ActorArtifact, Artifact, SilContractArtifact};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TemplateFrameLengths {
    pub prefix: usize,
    pub state: usize,
    pub suffix: usize,
    pub total: usize,
}

impl fmt::Display for TemplateFrameLengths {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "prefix length {}, state length {}, suffix length {}, total length {}",
            self.prefix, self.state, self.suffix, self.total
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TemplateFrameVerificationError {
    #[error("actor `{actor}` references missing embedded Sil contract `{contract}`")]
    MissingContract { actor: String, contract: String },
    #[error(
        "actor `{actor}` contract `{contract}` has an invalid compiled state span: offset {offset}, state length {state_len}, total length {total_len}"
    )]
    InvalidStateSpan { actor: String, contract: String, offset: usize, state_len: usize, total_len: usize },
    #[error(
        "conservative frame rule found an ambiguity between actors `{first_actor}` ({first_frame}) and `{second_actor}` ({second_frame})"
    )]
    Ambiguous { first_actor: String, first_frame: TemplateFrameLengths, second_actor: String, second_frame: TemplateFrameLengths },
}

struct ActorFrame<'a> {
    actor: &'a str,
    prefix: &'a [u8],
    state_len: usize,
    suffix: &'a [u8],
    total_len: usize,
}

impl ActorFrame<'_> {
    fn lengths(&self) -> TemplateFrameLengths {
        TemplateFrameLengths { prefix: self.prefix.len(), state: self.state_len, suffix: self.suffix.len(), total: self.total_len }
    }

    fn conflicts_with(&self, other: &Self) -> bool {
        self.total_len == other.total_len
            && (self.prefix.starts_with(other.prefix) || other.prefix.starts_with(self.prefix))
            && (self.suffix.ends_with(other.suffix) || other.suffix.ends_with(self.suffix))
    }
}

pub(crate) fn verify(artifact: &Artifact) -> Result<(), TemplateFrameVerificationError> {
    let mut actors = artifact.argent.actors.iter().collect::<Vec<_>>();
    actors.sort_by(|left, right| left.name.cmp(&right.name));

    let frames = actors
        .into_iter()
        .map(|actor| {
            let contract = artifact.sil_abi.contract(&actor.abi.contract).ok_or_else(|| {
                TemplateFrameVerificationError::MissingContract { actor: actor.name.clone(), contract: actor.abi.contract.clone() }
            })?;
            extract_frame(actor, contract)
        })
        .collect::<Result<Vec<_>, _>>()?;

    for (index, first) in frames.iter().enumerate() {
        for second in &frames[index + 1..] {
            if first.conflicts_with(second) {
                return Err(TemplateFrameVerificationError::Ambiguous {
                    first_actor: first.actor.to_string(),
                    first_frame: first.lengths(),
                    second_actor: second.actor.to_string(),
                    second_frame: second.lengths(),
                });
            }
        }
    }

    Ok(())
}

fn extract_frame<'a>(
    actor: &'a ActorArtifact,
    contract: &'a SilContractArtifact,
) -> Result<ActorFrame<'a>, TemplateFrameVerificationError> {
    let compiled = &contract.compiled;
    let offset = compiled.state_span.offset;
    let state_len = compiled.state_span.len;
    let total_len = compiled.bytecode.len();
    let Some(state_end) = offset.checked_add(state_len).filter(|state_end| *state_end <= total_len) else {
        return Err(TemplateFrameVerificationError::InvalidStateSpan {
            actor: actor.name.clone(),
            contract: actor.abi.contract.clone(),
            offset,
            state_len,
            total_len,
        });
    };

    Ok(ActorFrame {
        actor: &actor.name,
        prefix: &compiled.bytecode[..offset],
        state_len,
        suffix: &compiled.bytecode[state_end..],
        total_len,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{
        ARTIFACT_SCHEMA_VERSION, ActorAbiRefArtifact, ArgentArtifact, CompiledContractArtifact, GeneratorArtifact,
        InterfaceSetArtifact, RuntimeStateArtifact, SIL_ABI_SCHEMA_VERSION, SilAbiArtifact, StateSpanArtifact, TemplatePlanArtifact,
    };

    struct TestFrame<'a> {
        actor: &'a str,
        prefix: &'a [u8],
        state_len: usize,
        suffix: &'a [u8],
    }

    #[test]
    fn rejects_identical_frames_despite_distinct_claimed_identities() {
        let artifact = artifact_with_frames(&[
            TestFrame { actor: "Zulu", prefix: &[1, 2], state_len: 2, suffix: &[8, 9] },
            TestFrame { actor: "Alpha", prefix: &[1, 2], state_len: 2, suffix: &[8, 9] },
        ]);

        let alpha = artifact.sil_abi.contract("contract_Alpha").unwrap();
        let zulu = artifact.sil_abi.contract("contract_Zulu").unwrap();
        assert_ne!(alpha.compiled.template_hash, zulu.compiled.template_hash);

        let error = artifact.verify_template_frames().expect_err("identical frames must be rejected");
        assert_eq!(
            error,
            TemplateFrameVerificationError::Ambiguous {
                first_actor: "Alpha".to_string(),
                first_frame: TemplateFrameLengths { prefix: 2, state: 2, suffix: 2, total: 6 },
                second_actor: "Zulu".to_string(),
                second_frame: TemplateFrameLengths { prefix: 2, state: 2, suffix: 2, total: 6 },
            }
        );
        assert!(error.to_string().contains("conservative frame rule found an ambiguity"));
    }

    #[test]
    fn rejects_shifted_state_boundaries() {
        let artifact = artifact_with_frames(&[
            TestFrame { actor: "A", prefix: &[1, 2, 3], state_len: 3, suffix: &[7, 8, 9, 10] },
            TestFrame { actor: "B", prefix: &[1, 2, 3, 4], state_len: 3, suffix: &[8, 9, 10] },
        ]);

        artifact.verify_template_frames().expect_err("the documented shifted-boundary frames must be rejected");
    }

    #[test]
    fn accepts_unequal_complete_lengths() {
        let artifact = artifact_with_frames(&[
            TestFrame { actor: "A", prefix: &[1], state_len: 1, suffix: &[9] },
            TestFrame { actor: "B", prefix: &[1], state_len: 2, suffix: &[9] },
        ]);

        artifact.verify_template_frames().expect("unequal complete lengths cannot conflict");
    }

    #[test]
    fn accepts_incompatible_prefixes() {
        let artifact = artifact_with_frames(&[
            TestFrame { actor: "A", prefix: &[1, 2], state_len: 1, suffix: &[9] },
            TestFrame { actor: "B", prefix: &[1, 3], state_len: 1, suffix: &[9] },
        ]);

        artifact.verify_template_frames().expect("incompatible prefixes cannot conflict");
    }

    #[test]
    fn accepts_incompatible_suffixes() {
        let artifact = artifact_with_frames(&[
            TestFrame { actor: "A", prefix: &[1], state_len: 1, suffix: &[8, 9] },
            TestFrame { actor: "B", prefix: &[1], state_len: 1, suffix: &[7, 9] },
        ]);

        artifact.verify_template_frames().expect("incompatible suffixes cannot conflict");
    }

    #[test]
    fn intentionally_rejects_the_documented_conservative_false_positive_shape() {
        let artifact = artifact_with_frames(&[
            TestFrame { actor: "A", prefix: &[1, 2, 3, 4], state_len: 1, suffix: &[9] },
            TestFrame { actor: "B", prefix: &[1], state_len: 1, suffix: &[6, 7, 8, 9] },
        ]);

        artifact.verify_template_frames().expect_err("the conservative rule intentionally rejects this shape");
    }

    #[test]
    fn ignores_embedded_contracts_outside_the_local_actor_set() {
        let mut artifact = artifact_with_frames(&[TestFrame { actor: "Local", prefix: &[1], state_len: 1, suffix: &[9] }]);
        let foreign = contract_for(&TestFrame { actor: "Foreign", prefix: &[1], state_len: 1, suffix: &[9] }, 99);
        artifact.sil_abi.contracts.insert("contract_Foreign".to_string(), foreign);

        artifact.verify_template_frames().expect("an unreferenced linked-app contract is outside the local actor set");
    }

    #[test]
    fn rejects_missing_contracts_without_panicking() {
        let mut artifact = artifact_with_frames(&[TestFrame { actor: "A", prefix: &[1], state_len: 1, suffix: &[9] }]);
        artifact.argent.actors[0].abi.contract = "missing".to_string();

        assert!(matches!(
            artifact.verify_template_frames(),
            Err(TemplateFrameVerificationError::MissingContract { actor, contract })
                if actor == "A" && contract == "missing"
        ));
    }

    #[test]
    fn rejects_invalid_state_spans_without_panicking() {
        let mut artifact = artifact_with_frames(&[TestFrame { actor: "A", prefix: &[1], state_len: 1, suffix: &[9] }]);
        artifact.sil_abi.contracts.get_mut("contract_A").unwrap().compiled.state_span =
            StateSpanArtifact { offset: usize::MAX, len: 1 };

        assert!(matches!(
            artifact.verify_template_frames(),
            Err(TemplateFrameVerificationError::InvalidStateSpan {
                actor,
                contract,
                offset: usize::MAX,
                state_len: 1,
                total_len: 3,
            }) if actor == "A" && contract == "contract_A"
        ));
    }

    fn artifact_with_frames(frames: &[TestFrame<'_>]) -> Artifact {
        let actors = frames
            .iter()
            .map(|frame| ActorArtifact {
                name: frame.actor.to_string(),
                state: format!("{}State", frame.actor),
                abi: ActorAbiRefArtifact { contract: format!("contract_{}", frame.actor) },
                leader_for: Vec::new(),
                entries: Vec::new(),
            })
            .collect();
        let contracts = frames
            .iter()
            .enumerate()
            .map(|(index, frame)| (format!("contract_{}", frame.actor), contract_for(frame, index as u8)))
            .collect();

        Artifact {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            id: String::new(),
            generator: GeneratorArtifact { name: "test".to_string(), version: "0".to_string() },
            app: "Test".to_string(),
            dependencies: Vec::new(),
            root: String::new(),
            modules: Vec::new(),
            argent: ArgentArtifact {
                templates: Vec::new(),
                template_plan: TemplatePlanArtifact::default(),
                interfaces: InterfaceSetArtifact::default(),
                states: Vec::new(),
                state_expansions: Vec::new(),
                actor_enums: Vec::new(),
                actors,
            },
            sil_abi: SilAbiArtifact {
                schema_version: SIL_ABI_SCHEMA_VERSION,
                compiler_version: "test".to_string(),
                structs: BTreeMap::new(),
                contracts,
            },
        }
    }

    fn contract_for(frame: &TestFrame<'_>, identity: u8) -> SilContractArtifact {
        let mut bytecode = frame.prefix.to_vec();
        bytecode.resize(bytecode.len() + frame.state_len, 0);
        bytecode.extend_from_slice(frame.suffix);

        SilContractArtifact {
            source_path: format!("sil/{}.sil", frame.actor),
            runtime_state: RuntimeStateArtifact { source: format!("{}State", frame.actor), fields: Vec::new() },
            entries: BTreeMap::new(),
            cov_decl_to_abi: BTreeMap::new(),
            delegate_entry_abi: None,
            compiled: CompiledContractArtifact {
                bytecode,
                template_hash: [identity; 32],
                state_span: StateSpanArtifact { offset: frame.prefix.len(), len: frame.state_len },
            },
        }
    }
}
