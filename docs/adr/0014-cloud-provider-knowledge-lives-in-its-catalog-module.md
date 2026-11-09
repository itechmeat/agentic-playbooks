---
decided: 2026-11-09
decision-makers: apb maintainers
---

# Cloud provider knowledge lives in its catalog module

## Applies to

Every fact about a cloud model gateway apb knows (today NEAR AI Cloud):
its base URL and public listing URL, the name of its key variable, the
shape of its listing, and the rule that turns a row's owner into a privacy
class.

- Those facts live only in the provider's module under
  `apb_core::cloud_models` (`near_ai.rs`), registered in `all()`. The
  catalog cache, the refresh and the one check every surface shares
  (`cloud_models::check`) are generic over the registry.
- Facts about how an agent reaches a provider (dsh's `llm-pi-ai` row shape,
  the route spelling `nearai/<id>`) live only in that agent's module, as
  decision record 0005 says for every agent fact.
- The link between the two is one declaration on the agent's model rules:
  `ModelPolicy::cloud_routes`, a list of `(provider, prefix)` pairs, with
  `ModelPolicy::cloud_model` reading `<prefix>/<id>[@<effort>]`. No other
  code matches on a provider id or a route prefix.
- A run snapshots the row (`ResolvedInvocation::cloud`), so post-start reads
  come from the manifest (decision record 0002), and the agent's run-scoped
  patch carries it (decision record 0013).

## Why

A provider's URLs, key name and owner list change on the provider's
schedule, not an agent's. Keeping them in one module means a new gateway
or a changed owner list is one file and its tests, and an agent that gains
a route to an existing gateway is one declaration. Scattering them over the
model lists, the patch writer, the cost basis and the decisions loader
would let them drift apart, as decision record 0005 found for agent facts.

## Checked by

- apb-core `agents::dsh::tests::the_model_rules_list_deepseek_and_accept_other_routes`
- apb-core `agents::dsh::model::tests::the_cloud_route_and_dshs_parser_agree_on_every_catalog_id`
- apb-core `agents::dsh::model::tests::a_near_step_patch_carries_the_provider_row`
- apb-core `agents::dsh::model::tests::a_near_step_without_a_row_writes_the_model_row_alone`
- apb-core `cloud_models::near_ai::tests::owners_map_to_classes`
