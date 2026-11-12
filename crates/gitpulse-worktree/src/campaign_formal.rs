//! Formal state-machine model and runtime invariant verifier for multi-agent
//! `GitPulse` worktree campaigns, re-exported from [`gitpulse_lib::engine::campaign_formal`]
//! so the CLI (`gitpulse-worktree`) and MCP server (`gitpulse-mcp`) share one
//! canonical owner.

pub use gitpulse_lib::engine::campaign_formal::*;

// Bounded model-checking harnesses for the Kani Rust Verifier (`cargo kani --manifest-path crates/gitpulse-worktree/Cargo.toml`).
#[cfg(kani)]
mod kani_proofs {
    use super::*;

    fn symbolic_scope(is_dir: bool, choice: u8) -> LeaseScope {
        let path = match choice {
            0 => String::from("a"),
            1 => String::from("b"),
            2 => String::from("ab"),
            3 => String::from("a/b"),
            4 => String::from("a/c"),
            _ => String::from("a/b/c"),
        };
        if is_dir {
            LeaseScope::DirSubtree(path)
        } else {
            LeaseScope::FileExact(path)
        }
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn kani_scopes_overlap_is_symmetric() {
        let dir_a: bool = kani::any();
        let dir_b: bool = kani::any();
        let choice_a: u8 = kani::any();
        let choice_b: u8 = kani::any();
        kani::assume(choice_a < 6 && choice_b < 6);
        let a = symbolic_scope(dir_a, choice_a);
        let b = symbolic_scope(dir_b, choice_b);
        assert_eq!(scopes_overlap(&a, &b), scopes_overlap(&b, &a));
        assert!(scopes_overlap(&a, &a));
    }

    #[kani::proof]
    #[kani::unwind(35)]
    fn kani_ct_eq_32_is_exact() {
        let a: [u8; 32] = kani::any();
        let b: [u8; 32] = kani::any();
        assert_eq!(ct_eq_32(&a, &b), a == b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhaustive_multi_lane_campaign_schedule_model_checker() {
        let scopes = [
            LeaseScope::FileExact("src/a.rs".to_string()),
            LeaseScope::DirSubtree("src".to_string()),
            LeaseScope::FileExact("src/b.rs".to_string()),
        ];

        let mut schedules_checked = 0usize;
        let mut successful_commits = 0usize;
        let mut rejected_attacks = 0usize;

        for s0 in 0..6 {
            for s1 in 0..6 {
                for s2 in 0..6 {
                    for s3 in 0..6 {
                        let mut ctrl = CampaignController::new("oid-0");
                        ctrl.register_lane("w1", LaneRole::Worker).expect("w1");
                        ctrl.register_lane("w2", LaneRole::Worker).expect("w2");
                        ctrl.register_lane("integ", LaneRole::Integrator)
                            .expect("integ");

                        let mut last_prop: Option<u64> = None;
                        let mut oid_seq = 0u64;

                        for step in [s0, s1, s2, s3] {
                            let prev_bulletin = ctrl.bulletin().clone();
                            match step {
                                0 => {
                                    if ctrl
                                        .acquire_leases(
                                            "w1",
                                            LeaseMode::Write,
                                            vec![scopes[0].clone()],
                                        )
                                        .is_ok()
                                    {
                                        let cur = ctrl.target_oid().to_string();
                                        if let Ok(pid) =
                                            ctrl.stage_proposal("w1", &cur, &["src/a.rs"])
                                        {
                                            last_prop = Some(pid);
                                        }
                                    } else {
                                        rejected_attacks += 1;
                                    }
                                }
                                1 => {
                                    if ctrl
                                        .acquire_leases(
                                            "w2",
                                            LeaseMode::Write,
                                            vec![scopes[1].clone()],
                                        )
                                        .is_ok()
                                    {
                                        let cur = ctrl.target_oid().to_string();
                                        if let Ok(pid) =
                                            ctrl.stage_proposal("w2", &cur, &["src/b.rs"])
                                        {
                                            last_prop = Some(pid);
                                        }
                                    } else {
                                        rejected_attacks += 1;
                                    }
                                }
                                2 => {
                                    let cur = ctrl.target_oid().to_string();
                                    if ctrl.stage_proposal("w1", &cur, &["outside/x.rs"]).is_err() {
                                        rejected_attacks += 1;
                                    }
                                    if let Some(pid) = last_prop {
                                        assert!(ctrl
                                            .commit_target_branch("w1", pid, &cur, "forged", true)
                                            .is_err());
                                        rejected_attacks += 1;
                                    }
                                }
                                3 => {
                                    let _ = ctrl.post_bulletin("w1", "status", "checkpoint");
                                }
                                4 => {
                                    if let Some(pid) = last_prop {
                                        let cur = ctrl.target_oid().to_string();
                                        oid_seq += 1;
                                        let next_oid = format!("oid-{oid_seq}");
                                        if ctrl
                                            .commit_target_branch(
                                                "integ", pid, &cur, &next_oid, true,
                                            )
                                            .is_ok()
                                        {
                                            successful_commits += 1;
                                        } else {
                                            rejected_attacks += 1;
                                        }
                                    }
                                }
                                _ => {
                                    let _ = ctrl.release_lane_leases("w1");
                                }
                            }

                            ctrl.bulletin()
                                .verify_extension_of(&prev_bulletin)
                                .expect("bulletin must extend previous snapshot monotonically");
                            ctrl.check_global_invariants("oid-0").expect(
                                "global campaign invariants must hold at every reachable state",
                            );
                        }
                        schedules_checked += 1;
                    }
                }
            }
        }
        assert_eq!(schedules_checked, 1296);
        assert!(
            successful_commits > 0 && rejected_attacks > 0,
            "non-vacuity witness failed: commits={successful_commits} rejections={rejected_attacks}"
        );
    }

    #[test]
    fn aba_target_oid_cycle_and_empty_oid_are_rejected() {
        assert!(matches!(
            CampaignController::try_new(""),
            Err(CampaignViolation::InvalidRevisionOid(_))
        ));
        let broken = CampaignController::new("");
        assert!(broken.check_global_invariants("").is_err());

        let mut ctrl = CampaignController::try_new("oid-0").expect("valid genesis");
        ctrl.register_lane("w1", LaneRole::Worker).expect("w1");
        ctrl.register_lane("w2", LaneRole::Worker).expect("w2");
        ctrl.register_lane("integ", LaneRole::Integrator)
            .expect("integ");

        ctrl.acquire_leases(
            "w1",
            LeaseMode::Write,
            vec![LeaseScope::file("src/a.rs").expect("scope")],
        )
        .expect("w1 lease");
        ctrl.acquire_leases(
            "w2",
            LeaseMode::Write,
            vec![LeaseScope::file("src/b.rs").expect("scope")],
        )
        .expect("w2 lease");

        let p1 = ctrl
            .stage_proposal("w1", "oid-0", &["src/a.rs"])
            .expect("p1");
        let p2 = ctrl
            .stage_proposal("w2", "oid-0", &["src/b.rs"])
            .expect("p2");

        assert!(matches!(
            ctrl.commit_target_branch("integ", p2, "oid-0", "", true),
            Err(CampaignViolation::InvalidRevisionOid(_))
        ));
        assert!(matches!(
            ctrl.commit_target_branch("integ", p2, "oid-0", "oid-0", true),
            Err(CampaignViolation::TargetBranchCycle { .. })
        ));

        ctrl.commit_target_branch("integ", p2, "oid-0", "oid-1", true)
            .expect("p2 commit");

        ctrl.acquire_leases(
            "w2",
            LeaseMode::Write,
            vec![LeaseScope::file("src/b.rs").expect("scope")],
        )
        .expect("w2 lease 2");
        let p3 = ctrl
            .stage_proposal("w2", "oid-1", &["src/b.rs"])
            .expect("p3");
        assert!(matches!(
            ctrl.commit_target_branch("integ", p3, "oid-1", "oid-0", true),
            Err(CampaignViolation::TargetBranchCycle { .. })
        ));

        assert!(matches!(
            ctrl.commit_target_branch("integ", p1, "oid-1", "oid-2", true),
            Err(CampaignViolation::StaleBaseRevision { .. })
        ));
        ctrl.check_global_invariants("oid-0")
            .expect("invariants intact");
    }

    #[test]
    fn persisted_campaign_state_enforces_append_only_bulletin_and_invariants() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_path = dir.path().join("gitpulse").join("campaign.json");

        let mut ctrl = CampaignController::try_new("abc1234").expect("genesis");
        ctrl.register_lane("worker-1", LaneRole::Worker)
            .expect("worker");
        ctrl.register_lane("integrator", LaneRole::Integrator)
            .expect("integrator");
        ctrl.post_bulletin("worker-1", "handoff", "starting auth refactor")
            .expect("bulletin 1");
        ctrl.save_verified_atomic(&state_path).expect("save 1");

        let loaded = CampaignController::load_verified(&state_path).expect("load 1");
        assert_eq!(loaded, ctrl);

        let mut rewritten = CampaignController::try_new("abc1234").expect("genesis");
        rewritten
            .register_lane("worker-1", LaneRole::Worker)
            .expect("worker");
        rewritten
            .post_bulletin("worker-1", "handoff", "FORGED HISTORY")
            .expect("forged");
        let err = rewritten
            .save_verified_atomic(&state_path)
            .expect_err("must reject bulletin rewrite");
        assert!(err.contains("bulletin monotonicity violation"), "{err}");

        ctrl.post_bulletin("integrator", "ack", "lease approved")
            .expect("bulletin 2");
        ctrl.save_verified_atomic(&state_path).expect("save 2");
        assert_eq!(
            CampaignController::load_verified(&state_path)
                .expect("load 2")
                .bulletin()
                .entries()
                .len(),
            2
        );
    }
}
