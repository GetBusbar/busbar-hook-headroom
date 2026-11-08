// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//! Test host over the actual typed hook loader. No plugin implementation is imported.
use busbar_contract::abi::host::hook::{DecideFrame, DecideView};
use busbar_contract::abi::mechanism::call::Outcome;
use busbar_contract::hook_calls::{Answered, HookAxis, HookCalls};
use busbar_contract::hooks::{
    Candidate, HookStatus, RoutingContext, RoutingDecision, RoutingRequest, TransformOutcome,
};
use busbar_plugin_loader::dispatch::kinds::hook::Hook;
use busbar_plugin_loader::dispatch::{
    Bind, ConnTable, Dispatcher, LinkedRow, NoSink, load_dropped,
};
use std::{sync::Arc, time::Duration};

pub fn plugin_path() -> std::path::PathBuf {
    let p = std::path::PathBuf::from(
        std::env::var_os("BUSBAR_PLUGIN_CDYLIB")
            .expect("build Headroom and set BUSBAR_PLUGIN_CDYLIB to its fresh native library"),
    );
    assert!(
        p.is_absolute() && p.is_file(),
        "missing native Headroom image: {}",
        p.display()
    );
    p
}
fn dispatcher() -> Arc<Dispatcher> {
    Arc::new(Dispatcher::new(
        Default::default(),
        busbar_contract::services::test_caller_binding(),
    ))
}
pub struct Policy(Arc<dyn HookCalls>);
impl Policy {
    pub fn dropped(settings: &str) -> Self {
        let d = dispatcher();
        let rebind_d = d.clone();
        let rebind = move || {
            let row = LinkedRow::of(busbar_hook_headroom::door).map_err(|e| e.to_string())?;
            load_dropped::<Hook>(
                &plugin_path(),
                &row.statement,
                Bind {
                    instance: "headroom".into(),
                    max_inflight_cap: 64,
                    sink: Arc::new(NoSink),
                    dispatcher: rebind_d.adopter(),
                    conns: ConnTable::NoNeeds,
                },
            )
            .map_err(|e| e.to_string())
        };
        let plugin = rebind().expect("native Headroom door admits");
        Self(Arc::new(
            busbar_plugin_loader::hook_door::HookInstance::open(
                plugin,
                d,
                settings.as_bytes(),
                rebind,
            )
            .expect("native Headroom opens"),
        ))
    }
    pub fn registered(registry: &busbar_plugin_loader::PluginRegistry, settings: &str) -> Self {
        let row = registry.resolve("headroom").expect("registered alias");
        let linked = if row.linked() {
            vec![busbar_hook_headroom::door as busbar_contract::abi::mechanism::door::DoorFn]
        } else {
            vec![]
        };
        let axis =
            busbar_plugin_loader::hook_door::HookRows::new(&linked, Some(registry), dispatcher())
                .expect("validated hook axis");
        Self(
            axis.open(
                "headroom",
                "headroom",
                &serde_json::from_str(settings).unwrap(),
                Duration::from_secs(5),
            )
            .expect("registered hook opens"),
        )
    }
    pub fn name(&self) -> &str {
        self.0.name()
    }
    pub async fn transform(&self, req: &RoutingRequest<'_>, budget: Duration) -> TransformOutcome {
        let ctx = RoutingContext {
            pool: req.pool,
            budget_remaining: None,
            budget: &[],
        };
        let input = DecideFrame::first(DecideView::build(req, &[], &ctx));
        match self.0.transform(input, budget).await {
            Answered::Answer {
                outcome: Outcome::Ready,
                out,
                frame,
                ..
            } => {
                if out.verbs == busbar_contract::abi::hook::VERB_ABSTAIN {
                    return TransformOutcome::Abstain;
                }
                assert_eq!(out.verbs, busbar_contract::abi::hook::VERB_REWRITE);
                let json = serde_json::from_slice(&frame.rewrite(out.rewrite_written))
                    .expect("rewrite JSON");
                TransformOutcome::Rewrite(
                    busbar_contract::hook_wire::reply::parse_rewrite(&json).expect("typed rewrite"),
                )
            }
            other => panic!("actual Headroom transform failed: {other:?}"),
        }
    }
    pub async fn decide(
        &self,
        req: &RoutingRequest<'_>,
        candidates: &[Candidate<'_>],
        ctx: &RoutingContext<'_>,
        budget: Duration,
    ) -> Result<RoutingDecision, String> {
        match self
            .0
            .decide(
                DecideFrame::first(DecideView::build(req, candidates, ctx)),
                budget,
            )
            .await
        {
            Answered::Answer {
                outcome: Outcome::Ready,
                out,
                ..
            } => {
                assert_eq!(
                    out.verbs,
                    busbar_contract::abi::hook::VERB_ABSTAIN,
                    "Headroom does not rank or reject"
                );
                Ok(RoutingDecision::Abstain)
            }
            other => Err(format!("actual Headroom decide failed: {other:?}")),
        }
    }
    pub async fn configure(
        &self,
        name: &str,
        settings: &serde_json::Map<String, serde_json::Value>,
        version: u64,
        budget: Duration,
    ) -> Result<(), String> {
        self.0
            .configure(
                name,
                &serde_json::to_string(settings).unwrap(),
                version,
                budget,
            )
            .await
    }
    pub async fn describe(&self, budget: Duration) -> Option<serde_json::Value> {
        let bytes = self.0.describe(budget).await?;
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .unwrap()
            .get("schema")
            .cloned()
    }
    pub async fn status(&self, budget: Duration) -> Option<HookStatus> {
        let bytes = self.0.status(budget).await?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let s = value.get("status")?;
        Some(HookStatus {
            settings_version: s["settings_version"].as_u64(),
            settings: s["settings"].as_object().cloned(),
            metrics: s["metrics"].as_array().cloned(),
        })
    }
}
