//! The registry of every validator code: the code, the severities it may
//! be reported at, and a one-line meaning. The validator emits no code that
//! is not here (`the_registry_is_what_the_validator_can_emit` proves it from
//! the sources), and a code keeps its meaning and its severities through
//! 1.x (`docs/COMPATIBILITY.md`, the freeze of `validator-codes.txt`).

use super::Severity;

/// Every active validator code: the code, the severities it may be
/// reported at, and a one-line meaning. The validator emits no code that
/// is not here (a test proves it).
pub const CODES: &[(&str, &[Severity], &str)] = &[
    ("V01", &[Severity::Error], "the playbook has no start node"),
    ("V02", &[Severity::Error], "a duplicate param name"),
    (
        "V03",
        &[Severity::Error],
        "the playbook has no start node or more than one",
    ),
    (
        "V04",
        &[Severity::Error],
        "the start node has an incoming edge",
    ),
    (
        "V05",
        &[Severity::Error],
        "a finish node has an outgoing edge",
    ),
    (
        "V06",
        &[Severity::Error],
        "an edge references an unknown node",
    ),
    (
        "V07",
        &[Severity::Error],
        "a node is unreachable from start",
    ),
    (
        "V08",
        &[Severity::Warning],
        "from a reachable node no finish node is reachable",
    ),
    (
        "V09",
        &[Severity::Error],
        "a condition node covers neither success nor failure and declares no fallback edge",
    ),
    (
        "V10",
        &[Severity::Error],
        "a condition references a node that cannot execute before it",
    ),
    (
        "V11",
        &[Severity::Error],
        "a cycle has no edge with max_traversals and passes through no condition node with max_loops",
    ),
    (
        "V12",
        &[Severity::Error],
        "a script path leaves the version directory or a success_check script lives outside scripts/",
    ),
    (
        "V13",
        &[Severity::Error],
        "a template reference cannot be resolved",
    ),
    (
        "V14",
        &[Severity::Error],
        "an explicit project profile reference in a global playbook, or a profile that is not found",
    ),
    (
        "V17",
        &[Severity::Error],
        "a structured trigger field is malformed or too long",
    ),
    (
        "V18",
        &[Severity::Error],
        "an empty or null executors list has nothing to migrate",
    ),
    (
        "V19",
        &[Severity::Warning],
        "an agent_task or script node without expected_duration where progress weighting needs one",
    ),
    ("V20", &[Severity::Error], "a malformed expected_duration"),
    (
        "V21",
        &[Severity::Error],
        "a finish node binds a profile but has no prompt",
    ),
    (
        "V22",
        &[Severity::Error],
        "a sub-playbook reference id is empty or not a safe path segment",
    ),
    (
        "V23",
        &[Severity::Error],
        "a connector binding name, an accounts entry or a function name is malformed",
    ),
    (
        "V24",
        &[Severity::Error],
        "a node binds the same connector name more than once",
    ),
    (
        "V25",
        &[Severity::Error],
        "an accounts or functions list entry is empty or repeated",
    ),
    (
        "V26",
        &[Severity::Error],
        "a function allowlist names a function the connector manifest does not map",
    ),
    (
        "V27",
        &[Severity::Error],
        "cache: auto on a node kind the engine never caches",
    ),
    (
        "V28",
        &[Severity::Warning],
        "a cache or checkpoint setting this build ignores",
    ),
    (
        "V29",
        &[Severity::Error],
        "a protect, writes or read_only glob does not compile",
    ),
    (
        "V30",
        &[Severity::Error],
        "a max_traversals of 0 on an edge",
    ),
    (
        "V31",
        &[Severity::Error],
        "interactive companion fields on a node kind other than agent_task",
    ),
    (
        "V32",
        &[Severity::Error],
        "question_timeout_seconds without interactive: true, or missing where required",
    ),
    (
        "V33",
        &[Severity::Error],
        "a success_check on a node kind other than agent_task",
    ),
    (
        "V34",
        &[Severity::Error],
        "two edges from one node whose first-match routing keys are contradictory or identical",
    ),
    (
        "V35",
        &[Severity::Error],
        "defaults.on_failure names an unknown node",
    ),
    (
        "V36",
        &[Severity::Error],
        "an edge join value other than all or any",
    ),
    (
        "V37",
        &[Severity::Warning],
        "the incoming edges of one node disagree on the join mode",
    ),
    (
        "V38",
        &[Severity::Warning],
        "a template reads nodes.<id>.output or report where the source declares no outputs.fields",
    ),
    (
        "V39",
        &[Severity::Warning],
        "a review_status edge on a condition the source node never sets",
    ),
    (
        "V40",
        &[Severity::Warning],
        "a review_status edge whose node is not reachable before the review decides",
    ),
    (
        "V41",
        &[Severity::Error, Severity::Warning],
        "a goal that is not complete, or a success_check script that is malformed",
    ),
    (
        "V42",
        &[Severity::Error],
        "a connector binding that cannot do what it declares (an unknown account, an inbox rule that needs an inbox)",
    ),
    (
        "V43",
        &[Severity::Error],
        "a node grants inbox functions of a connector whose webhook wiring the installed manifest lacks",
    ),
    (
        "V44",
        &[Severity::Error],
        "continue_session names an unknown node, the node itself, or a node that is not an agent_task",
    ),
    (
        "V45",
        &[Severity::Warning],
        "a continue_session fork whose branch may run concurrently with the session",
    ),
    (
        "V46",
        &[Severity::Warning],
        "a node reads a field the source does not declare in outputs.fields (warning only where the source declares them)",
    ),
    (
        "V47",
        &[Severity::Error],
        "a template namespace a node kind does not take, or a workdir on a read-only node",
    ),
    (
        "V48",
        &[Severity::Error],
        "a check source names a node that is not an agent_task or script",
    ),
    (
        "V49",
        &[Severity::Error, Severity::Warning],
        "a malformed cache block (error), or a key that reads nothing and has no ttl (warning)",
    ),
    (
        "V50",
        &[Severity::Error],
        "a judge question shape is malformed (two questions writing one output field included)",
    ),
    (
        "V51",
        &[Severity::Error],
        "a judge question content problem (empty text, an option list that does not fit the kind)",
    ),
    (
        "V52",
        &[Severity::Error],
        "a judge threshold that does not read or is out of range",
    ),
    (
        "V53",
        &[Severity::Error],
        "an unknown judge on_unavailable value",
    ),
    (
        "V54",
        &[Severity::Error],
        "a judge state field that does not read",
    ),
    (
        "V55",
        &[Severity::Warning],
        "a judge without on_unavailable: a failing model ask fails the node (fail is the default)",
    ),
    (
        "V56",
        &[Severity::Warning],
        "a below threshold without the as-below wording",
    ),
    (
        "V57",
        &[Severity::Warning],
        "a judge option label holds digits the model would see as an answer",
    ),
    (
        "V58",
        &[Severity::Warning],
        "a judge question reads a whole transcript, which is clipped",
    ),
    (
        "V59",
        &[Severity::Error],
        "a judge edge that lacks the answer it compares, or compares a whole transcript",
    ),
    (
        "V60",
        &[Severity::Warning],
        "a yes-or-no judge routes more than eight edges",
    ),
    (
        "V61",
        &[Severity::Warning],
        "a judge profile that only on_unavailable: emulate would use, with another on_unavailable in force",
    ),
    (
        "V62",
        &[Severity::Error],
        "a malformed baseline_check block: no checks, a duplicate or malformed id, an unknown connector, an unknown only kind, a timeout out of range",
    ),
    (
        "V63",
        &[Severity::Error],
        "a baseline observation source that does not read (an unknown connector function, a script problem, a project_checks problem)",
    ),
    (
        "V64",
        &[Severity::Error],
        "a baseline rule that does not fit the observation type, or compares a non-number with count",
    ),
    (
        "V65",
        &[Severity::Error],
        "baseline connector args or complete_when template a namespace other than declared params.<name>",
    ),
    (
        "V66",
        &[Severity::Warning],
        "a baseline without a goal, or with only manual criteria",
    ),
    (
        "V67",
        &[Severity::Warning],
        "a script or project_checks baseline while an isolated node reads the same tree",
    ),
    (
        "V68",
        &[Severity::Error],
        "a node id that starts with @: those ids are reserved for the engine sites",
    ),
    (
        "V69",
        &[Severity::Error],
        "a binding sets on_unknown but none of the functions it allows takes an inbox read",
    ),
    (
        "V70",
        &[Severity::Error, Severity::Warning],
        "route: auto on an agent_task whose executor chain offers no tiers to pick from (an error on a finish, a warning on an agent)",
    ),
    (
        "V71",
        &[Severity::Error],
        "option_descriptions names a value that is not an offered option, or sits on a node without options",
    ),
    (
        "V72",
        &[Severity::Error],
        "auto_decide on a human_review that cannot take it (no options, an evidence-taking gate, a required field without a default)",
    ),
    (
        "V73",
        &[Severity::Error, Severity::Warning],
        "an agent step after a decision gate looks like shipping (merge, push, deploy, publish, an irreversible function): an error under auto_decide or unattended decide, a warning otherwise",
    ),
    (
        "V74",
        &[Severity::Warning],
        "a decision-model use without a decisions.yaml that enables it",
    ),
    (
        "V75",
        &[Severity::Error],
        "a protect glob that is empty, absolute, climbs out with .. or does not parse",
    ),
    (
        "V76",
        &[Severity::Warning],
        "a protect glob that matches no file in the project",
    ),
    (
        "V77",
        &[Severity::Error],
        "fork on a node kind that cannot fork",
    ),
    (
        "V78",
        &[Severity::Error],
        "fork.on_failure missing under fail_fast, present under wait, or naming an unknown node",
    ),
    (
        "V79",
        &[Severity::Error],
        "require: all_succeeded on an edge into a node that is no join",
    ),
    (
        "V80",
        &[Severity::Error],
        "an eval suite or case file does not load",
    ),
    (
        "V81",
        &[Severity::Error],
        "the eval suite cannot run: the refusal (a playbook with no connector facts, a double that cannot be built)",
    ),
    (
        "V82",
        &[Severity::Warning],
        "nodes wait for a person; an eval run stops there",
    ),
    (
        "V83",
        &[Severity::Warning],
        "no eval case of the suite applies to the playbook version",
    ),
    (
        "V84",
        &[Severity::Error],
        "a connector function an edge condition reads is granted irreversible",
    ),
    (
        "V85",
        &[Severity::Error],
        "where list: may sit and its numbers (above)",
    ),
    (
        "V86",
        &[Severity::Error],
        "the item source: exactly one of from and inline, a source that finishes before the node, a declared field (or items of a list node), where shapes (fie",
    ),
    (
        "V87",
        &[Severity::Error],
        "the check: one source, an output field among item_outputs, a script under scripts/, a read-only connector function, a valid accept_when (one of equals",
    ),
    (
        "V88",
        &[Severity::Error],
        "a granted write function without an idempotency declaration",
    ),
    (
        "V89",
        &[Severity::Error],
        "a judge check's criteria keys, min_confidence in (0, 1], on_unavailable",
    ),
    (
        "V90",
        &[Severity::Warning],
        "a node looks like a merge, push, deploy or publish step but declares no irreversible effect",
    ),
    (
        "V91",
        &[Severity::Error, Severity::Warning],
        "a profile reference that cannot be loaded (an error when only the project can fix it, a warning otherwise)",
    ),
    (
        "V92",
        &[Severity::Error],
        "a script, check or goal file the version directory does not carry where a run would look for it",
    ),
    (
        "V93",
        &[Severity::Warning],
        "a list without a check; rerun: not_passed on a node in no cycle",
    ),
    (
        "V94",
        &[Severity::Warning],
        "batch.parallel above the playbook's max_parallel",
    ),
    (
        "V95",
        &[Severity::Error],
        "a type: list goal criterion that names a node without list:",
    ),
    (
        "V96",
        &[Severity::Error],
        "budget declares neither tokens nor usd",
    ),
    (
        "V97",
        &[Severity::Error],
        "budget.tokens is 0, or budget.usd is not a finite amount greater than 0",
    ),
    (
        "V98",
        &[Severity::Warning],
        "a budget on a playbook with no agent node and no playbook node, so it never applies",
    ),
    (
        "V99",
        &[Severity::Error],
        "a secret: true param has a default: the secret would sit in the YAML",
    ),
    (
        "V100",
        &[Severity::Error],
        "a secret: true param is not type: text",
    ),
    (
        "V101",
        &[Severity::Warning],
        "audience on a finish node without a prompt",
    ),
    (
        "V102",
        &[Severity::Error],
        "a malformed criterion: a bad, duplicate or built-in id, a missing level, empty or too long text, hidden characters",
    ),
    (
        "V103",
        &[Severity::Error],
        "more than three criteria, or a subject.node that is unknown or not an agent_task",
    ),
    (
        "V104",
        &[Severity::Error],
        "task_type reads anything but params.*, or a secret param",
    ),
    (
        "V105",
        &[Severity::Error],
        "an unknown built-in, or a built-in switched off without a reason",
    ),
    (
        "V106",
        &[Severity::Error, Severity::Warning],
        "promote_supervisor_patches or trial_candidates does not read (an unknown word or key, a value out of range, two forms mixed) and falls back to its def",
    ),
    (
        "V107",
        &[Severity::Error, Severity::Warning],
        "promote_supervisor_patches: { when: quality } without a quality: block, with no criterion in force, or with the built-in outcome switched off",
    ),
    (
        "V108",
        &[Severity::Error],
        "a malformed block: an unknown show, min_runs outside 1 to 20, max_shown outside 1 to 10",
    ),
    (
        "V109",
        &[Severity::Warning],
        "the block shows problems, but the playbook has no agent_task node to show them to",
    ),
    ("V110", &[Severity::Error], "an unknown criteria_grader"),
    (
        "V111",
        &[Severity::Warning],
        "criteria_grader: model with no criteria of the playbook's own: the model grader has nothing to grade",
    ),
    (
        "V112",
        &[Severity::Warning],
        "full with the network left implicit: say network: open or network: none",
    ),
    (
        "V113",
        &[Severity::Error],
        "isolation together with connectors: the sandbox withholds both the connector credentials and the journal the send ledger writes; an MCP connector also",
    ),
    (
        "V114",
        &[Severity::Error],
        "an extra path that is relative, contains .., is / or ~, or reaches a credential location (~/.ssh, ~/.gnupg, ~/.aws, ~/.config/gh, ~/.config/apb, ~/.do",
    ),
    (
        "V115",
        &[Severity::Error],
        "network, read_only or writable with level none",
    ),
    (
        "V116",
        &[Severity::Error],
        "from names an existing agent_task or script node, not a list node, from which the publish node is reachable",
    ),
    (
        "V117",
        &[Severity::Error],
        "remote is a remote name ([A-Za-z0-9._-], not led by -); branch reads only params.*, run.id and nodes.<id>.output[.<field>] (not a report or a review d",
    ),
    (
        "V118",
        &[Severity::Error],
        "A publish node does not take list, fork, cache, success_check, route, completion_check, option_descriptions, auto_decide_ok or inputs/outputs (auto_de",
    ),
    (
        "V119",
        &[Severity::Warning],
        "The playbook publishes and a node runs an agent without isolation: full, or scripts run outside the sandbox (script nodes, baseline script and project",
    ),
    (
        "V120",
        &[Severity::Error, Severity::Warning],
        "publish_only: true and a node that runs an agent without isolation: full (an agent task, a finish with a prompt, a judge that emulates; the message na",
    ),
    (
        "V121",
        &[Severity::Error],
        "an edge id is malformed or used by two edges",
    ),
    (
        "V122",
        &[Severity::Warning],
        "edges with the same two ends carry different max_traversals: the traversal count belongs to the pair, so every traversal of either edge counts toward ",
    ),
    (
        "V123",
        &[Severity::Warning],
        "a fallback edge of a node that routes to one edge (it has a conditional or fallback edge and no plain one) is not that node's last outgoing edge; a fa",
    ),
    (
        "V124",
        &[Severity::Warning],
        "such a node has two edges without max_traversals with the same target, condition and fallback flag; only the first can ever be taken (plain fan-out ed",
    ),
    (
        "V125",
        &[Severity::Warning],
        "an output_field edge compares a judge node's field with a value it never writes: a choice question writes one of its options, a noul question true or ",
    ),
    (
        "V126",
        &[Severity::Error],
        "initial is not a duration, or is below 1s or above 7d",
    ),
    ("V127", &[Severity::Error], "factor is not between 1 and 10"),
    (
        "V128",
        &[Severity::Error],
        "max is not a duration, is below initial or above 7d",
    ),
    (
        "V129",
        &[Severity::Error],
        "a list node carries retry_delay",
    ),
    (
        "V130",
        &[Severity::Warning],
        "the delay is never used: the node's effective max_retries is 0, or no node defaults.retry_delay applies to (an agent_task without its own retry_delay,",
    ),
    (
        "V131",
        &[Severity::Error],
        "an output name does not match [a-z][a-z0-9_]* or is longer than 64 characters, or more than 64 outputs are declared",
    ),
    (
        "V132",
        &[Severity::Error],
        "an output has no source, a source names a node that does not exist, or a source sets both field and item_field",
    ),
    (
        "V133",
        &[Severity::Warning],
        "field is not one the node publishes (its outputs.fields, a list node's counts, a judge node's answers, a child's declared outputs), when the node decl",
    ),
    (
        "V134",
        &[Severity::Error],
        "item_field on a node that is not a list node",
    ),
    (
        "V135",
        &[Severity::Warning],
        "item_field is not one of the list's item_outputs, when it declares them",
    ),
    (
        "V136",
        &[Severity::Warning],
        "a human_review decision is routed only by bounded edges, and the review has no fallback edge and no unbounded edge that decision can take: once the ca",
    ),
    (
        "V137",
        &[Severity::Error],
        "read_only on a node that is not an agent_task or a script",
    ),
    ("V138", &[Severity::Error], "read_only on a list node"),
    (
        "V139",
        &[Severity::Warning],
        "read_only together with protect: the read-only check covers the whole tree, so protect adds nothing",
    ),
    (
        "V140",
        &[Severity::Warning],
        "a publish node names a read_only node as from: it commits nothing, so the publish pushes only what earlier nodes committed",
    ),
    (
        "V141",
        &[Severity::Error],
        "unattended on a node that is not a human_review, or decide names an option the review does not offer",
    ),
    (
        "V142",
        &[Severity::Error],
        "a node the chosen option reaches before the next human_review or publish declares or implies irreversible or secrets effects, may call a connector fun",
    ),
    (
        "V143",
        &[Severity::Error],
        "the chosen option is routed only by bounded edges: once the cap is spent the decision has no edge, and an unattended loop would have no way out",
    ),
    (
        "V144",
        &[Severity::Warning],
        "a review_status edge that can never match: its source is not a human_review (only a review has a decision), or its value is not one of the source's ef",
    ),
    (
        "V145",
        &[Severity::Warning],
        "a human_review option no edge can carry: no review_status edge with that value, no unconditional edge, no edge with another condition that may match, ",
    ),
    (
        "V146",
        &[Severity::Error],
        "the scenario file on its own: does not read or parse, unknown field, schema not 1, id not the file name or outside the grammar, malformed versions, a ",
    ),
    (
        "V147",
        &[Severity::Error],
        "an answer key: an unknown node, a kind the node does not take, a visit not positive or numbered twice, more than one default, a goal index that is not",
    ),
    (
        "V148",
        &[Severity::Error],
        "an answer value: a review or publish decision that is not an option, a judge question, option, level or probability the node does not allow, a judge e",
    ),
    (
        "V149",
        &[Severity::Error],
        "a param the playbook declares secret is set",
    ),
    (
        "V150",
        &[Severity::Error],
        "an expectation names an unknown node, edge or run output, a finish that is not a finish node, a join input that does not feed the join, an outcome or ",
    ),
    (
        "V151",
        &[Severity::Warning],
        "a visit answers more attempts than the node's max_retries allows (infrastructure and quota failures not counted)",
    ),
    (
        "V152",
        &[Severity::Error],
        "results on a node that settles no work of its own: anything but an agent_task, a script or a finish with a prompt",
    ),
    (
        "V153",
        &[Severity::Error],
        "a results path breaks the grammar: empty, absolute or holding a NUL; **; an empty, . or .. segment; * or ? outside the last segment; or one of the brackets and braces and the backslash",
    ),
    (
        "V154",
        &[Severity::Error],
        "more than 32 results entries on one node",
    ),
    (
        "V155",
        &[Severity::Error],
        "person_only on a node other than a human_review or a publish",
    ),
    (
        "V156",
        &[Severity::Error],
        "person_only with auto_decide or with unattended: { decide }, which would decide without a person (unattended: fail is fine)",
    ),
    (
        "V157",
        &[Severity::Error],
        "approves on a node other than a human_review; an empty results; a named node that is unknown, settles no result of its own (anything but an agent_task",
    ),
    (
        "V158",
        &[Severity::Error],
        "requires_approval on a node that does no work; a gate that is unknown, declares no approves or does not lead to the node; an of that is empty or names",
    ),
    (
        "V159",
        &[Severity::Error],
        "evidence on a node other than an agent_task; a from list that is empty, names an unknown node, a node that settles no result (anything but an agent_ta",
    ),
    (
        "V160",
        &[Severity::Error],
        "a search: true param is also secret: true",
    ),
    (
        "V161",
        &[Severity::Error],
        "a search: true param is named like a secret (api_token, password, client_secret, ...)",
    ),
    (
        "V162",
        &[Severity::Error],
        "more than 8 params are search: true",
    ),
    (
        "V163",
        &[Severity::Error],
        "a success_check names no scope glob or one that does not compile",
    ),
    (
        "V164",
        &[Severity::Error],
        "a malformed no_new_items: the node is unknown, not an agent_task or script, or is a list node; field or key is empty or holds a ., whitespace or a }; ",
    ),
    (
        "V165",
        &[Severity::Error],
        "a {{nodes.<id>.seen.<field>}} template with no no_new_items condition on that node and field, or two such conditions on them that disagree on key",
    ),
    (
        "V166",
        &[Severity::Warning],
        "the condition's node and the edge's source share no cycle, so the condition can never match",
    ),
    (
        "V167",
        &[Severity::Error],
        "a typed entry is malformed: an empty name or one holding ., {, } or whitespace; a name declared twice where one of the two is typed; values without ty",
    ),
    (
        "V168",
        &[Severity::Warning],
        "a reader cannot read a typed field's type: an output_field equals on a bool field other than true/false, on a number field no number reads as, on an e",
    ),
    (
        "V169",
        &[Severity::Error],
        "an output_field condition names no comparator or more than one of equals, lt, lte, gt, gte, or a threshold that is not a finite number",
    ),
    (
        "V170",
        &[Severity::Error],
        "writes or max_changed_files on a node that is not an agent_task or script, on a read_only node, on a list node or on a node with cache; writes: []; mo",
    ),
    (
        "V171",
        &[Severity::Warning],
        "two writers can run side by side and neither declares writes or max_changed_files",
    ),
    (
        "V172",
        &[Severity::Warning],
        "two sibling writers where at least one declares writes or max_changed_files: they run one after another",
    ),
    (
        "V173",
        &[Severity::Error],
        "on_green_before without prove_fails_first: true; a { script } map with neither scope nor prove_fails_first: true (write the bare string); prove_fails_",
    ),
    (
        "V174",
        &[Severity::Error],
        "stall_after outside 2 to 10 (on a node or defaults), or on a node that is not an agent_task, or on a list node",
    ),
    (
        "V175",
        &[Severity::Warning],
        "a node_status edge with equals: stalled whose node has no effective stall_after (it never matches), or that comes after an edge from the same source w",
    ),
    (
        "V176",
        &[Severity::Error],
        "a malformed run.previous_success reference, an unknown node in one, a read of the namespace in the run's worktree template, or a when_none value that ",
    ),
    (
        "V177",
        &[Severity::Error],
        "a pattern on a param that is not text, an empty pattern, one over 512 characters, or one that does not compile",
    ),
    (
        "V178",
        &[Severity::Error],
        "the param's default (or a scenario param value) does not match its pattern",
    ),
    (
        "V179",
        &[Severity::Error],
        "a follower renders untrusted text, can send outward and has no accept_untrusted",
    ),
    (
        "V180",
        &[Severity::Warning],
        "a follower renders untrusted text and reaches secrets (accepted or not)",
    ),
    (
        "V181",
        &[Severity::Error],
        "untrusted: true on a node that is not agent_task or script; a param both secret and untrusted; accept_untrusted on a node that is not a follower kind,",
    ),
    (
        "V182",
        &[Severity::Warning],
        "accept_untrusted on a node that renders no untrusted text or cannot send outward (nothing to accept)",
    ),
    (
        "V183",
        &[Severity::Error],
        "after is not a duration, or is under a minute or over 30 days",
    ),
    (
        "V184",
        &[Severity::Error],
        "grace is not a duration, or is under a minute or over a week",
    ),
    (
        "V185",
        &[Severity::Error],
        "the check's source is project_checks: the project's own tests days later measure the commits made since, not this run",
    ),
    (
        "V186",
        &[Severity::Error],
        "an observation's rule is a change rule",
    ),
    (
        "V187",
        &[Severity::Warning],
        "a binding without untrusted: true reaches an untrusted_output function of a known connector",
    ),
    (
        "V200",
        &[Severity::Error],
        "deadline is not a duration, is zero, under 1 minute or over 30 days",
    ),
    (
        "V210",
        &[Severity::Error],
        "the key is empty or over 500 characters, or reads something other than a declared non-secret params.<name> (a node output, an undeclared param, a secr",
    ),
    (
        "V211",
        &[Severity::Warning],
        "on_busy: supersede on a playbook whose effective effects include irreversible",
    ),
    (
        "V212",
        &[Severity::Error],
        "health sets no rule, consecutive_failures is outside 1 to 100, failure_rate.at_least outside (0, 1], window outside 2 to 200, min_runs outside 1 to wi",
    ),
    (
        "V220",
        &[Severity::Warning],
        "an executor chain reaches a model an agent step cannot run on",
    ),
    (
        "V221",
        &[Severity::Warning],
        "an executor chain spans more than one privacy class",
    ),
    (
        "V222",
        &[Severity::Warning],
        "an executor chain names a cloud model the catalog does not list",
    ),
    (
        "V240",
        &[Severity::Error],
        "pilot.items is outside 1 to 50, is not below items.max_items, or would run every item of an inline list",
    ),
    (
        "V241",
        &[Severity::Error],
        "stop_when sets no rule, needs_look_at_least is 0, needs_look_share is not above 0 and at most 1, or min_settled is 0 or given without needs_look_share",
    ),
    (
        "V242",
        &[Severity::Warning],
        "pilot.then: continue on a list without an item check: every item passes, so the gate would never be asked",
    ),
    (
        "V243",
        &[Severity::Error],
        "a malformed max_scope: no limit set, a limit of 0, a withheld option the gate does not offer, every offered option withheld, or no withhold on a gate ",
    ),
    (
        "V244",
        &[Severity::Error],
        "a malformed human_review.fields: names, duplicates, the reserved decision, enum shape, max_len misuse, a required option the gate does not offer, a li",
    ),
    (
        "V245",
        &[Severity::Error],
        "a malformed human_review.show: an empty or over-long label, an empty text, more than 10 entries",
    ),
    (
        "V246",
        &[Severity::Error],
        "a field required for an option the engine may choose without a default (or with a blank literal one) on a gate with auto_decide or unattended: { decid",
    ),
    (
        "V247",
        &[Severity::Warning],
        "a field default renders untrusted text",
    ),
    (
        "V249",
        &[Severity::Error],
        "unattended.after_attended_successes is outside 1 to 100",
    ),
    (
        "V250",
        &[Severity::Warning],
        "a playbook with a top-level unattended: block has an acting agent_task without a deterministic check (the readiness report lists the gap for every pla",
    ),
    (
        "V260",
        &[Severity::Error],
        "the source does not resolve: unknown connector, a connector without a webhook: block (an MCP tool connector included), unknown account, types on a con",
    ),
    (
        "V261",
        &[Severity::Error],
        "the playbook does not resolve (unknown id or version) or its lifecycle is draft or retired",
    ),
    (
        "V262",
        &[Severity::Error],
        "a mapped param is not declared or is secret: true; a param that reads event text ({{body.*}} or {{event.type}}) is not untrusted: true; a required par",
    ),
    (
        "V263",
        &[Severity::Error],
        "no budget: neither the rule's budget nor the playbook's budget: limits tokens or dollars, or the effective on_no_estimate is ask",
    ),
    (
        "V264",
        &[Severity::Error],
        "a limit out of range: max_per_hour not 1 to 500, an empty key, more than 32 params",
    ),
    (
        "V265",
        &[Severity::Warning],
        "the playbook can write through the connector and account the rule listens on with a function whose idempotency is not marker: its own writes come back",
    ),
    (
        "V266",
        &[Severity::Warning],
        "external_data: false: what arrives from outside is kept for learning",
    ),
    (
        "V267",
        &[Severity::Warning],
        "the effective on_no_estimate is pause while the rule's runs are external data (the default)",
    ),
    (
        "V280",
        &[Severity::Error],
        "D1 | an eval case's connectors: doubles are malformed, name a connector or function no binding site of the version binds, or leave a bound function wi",
    ),
    (
        "V281",
        &[Severity::Error],
        "D1 | a case's checks.connector_calls entry is malformed or names a function the case does not double",
    ),
    (
        "V283",
        &[Severity::Error],
        "a malformed list.skip: unless_changed lists no field, names a field twice, or names one that is empty or holds a . or whitespace",
    ),
    (
        "V284",
        &[Severity::Error],
        "a malformed wait_for: { type: inbox } block",
    ),
    (
        "V285",
        &[Severity::Error],
        "an inbox wait names a connector that is not installed, or one that has no webhook: block, so its inbox never receives anything; an MCP connector gets ",
    ),
    (
        "V286",
        &[Severity::Warning],
        "an inbox wait sets types but the connector's webhook block names no event type (WebhookSpec::names_event_type, crates/apb-core/src/connector/def.rs li",
    ),
    (
        "V287",
        &[Severity::Error],
        "two connector binding sites have the same id: a node named like another node's derived site (a#inbox beside the inbox wait node a, a#check beside the ",
    ),
    (
        "V288",
        &[Severity::Error],
        "list.skip on a node whose isolation is full or best_effort: skip on isolated nodes is not supported yet",
    ),
    (
        "V289",
        &[Severity::Error],
        "list.skip on an agent list whose prompt reads a value of the run other than a param or run.instruction (an earlier node's output, run.context, run.id)",
    ),
    (
        "V300",
        &[Severity::Warning],
        "a binding of an MCP connector names no functions list, so it grants every tool the manifest maps (functions: read_only counts as a list)",
    ),
    (
        "V301",
        &[Severity::Warning],
        "the connector's stdio command is an interpreter or a launcher (node, python3, sh, npx, uvx, docker and the like, a versioned name such as python3.12 i",
    ),
    (
        "V302",
        &[Severity::Error],
        "an MCP connector at an engine site (the baseline check, the follow-up check, a list item check or an inbox wait): those reads run from the drive or a ",
    ),
    (
        "V303",
        &[Severity::Error],
        "an http MCP connector whose auth header names a header the HTTP transport sets itself (mcp-protocol-version, mcp-method, mcp-name, content-type, accep",
    ),
    (
        "V320",
        &[Severity::Error],
        "D4.2 | baseline_check project_checks.junit is not a usable report glob",
    ),
    (
        "V340",
        &[Severity::Error],
        "a string of the definition references a secret: true param ({{params.<name>}}, with or without spaces inside the braces), mapping keys included",
    ),
];

/// Validator codes that no longer fire. A retired code keeps its number:
/// no new rule may take it (`docs/COMPATIBILITY.md`).
pub const RETIRED_CODES: &[(&str, &str)] = &[
    ("V15", "never emitted"),
    (
        "V16",
        "declared but not enforced isolation; isolation is enforced since 1.0.0-beta.1",
    ),
];

#[cfg(test)]
mod tests {
    use super::super::severity_word;
    use super::*;

    /// The sources of `crates/apb-core/src`, each cut at its first
    /// `#[cfg(test)]`, comments stripped.
    fn production_sources() -> Vec<(String, String)> {
        let mut out = Vec::new();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) => panic!("cannot read {}: {e}", dir.display()),
            };
            for entry in entries {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|x| x == "rs")
                    // This file is left out: its tables name every code.
                    && path.file_name().is_some_and(|x| x != "codes.rs")
                {
                    let text = std::fs::read_to_string(&path).expect("read source");
                    let cut = text.find("#[cfg(test)]").unwrap_or(text.len());
                    let stripped = text[..cut]
                        .lines()
                        .map(|l| l.trim_start())
                        .filter(|l| !l.starts_with("//"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    out.push((path.display().to_string(), stripped));
                }
            }
        }
        out
    }

    /// The codes a `"V<n>"` literal names, with the file it sits in.
    fn literal_codes(sources: &[(String, String)]) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for (file, text) in sources {
            for part in text.split('"') {
                // Only whole string literals reach here; a "V<n>" piece is a
                // literal's content.
                if part.len() >= 3
                    && part.starts_with('V')
                    && part[1..].bytes().all(|b| b.is_ascii_digit())
                {
                    out.insert(part.to_string());
                    let _ = file;
                }
            }
        }
        out
    }

    /// `name -> "V<n>"` for every `pub const <NAME>: &str = "V<n>";`.
    fn code_constants(sources: &[(String, String)]) -> std::collections::BTreeMap<String, String> {
        let mut out = std::collections::BTreeMap::new();
        for (_file, text) in sources {
            for line in text.lines() {
                let Some((left, right)) = line.split_once('=') else {
                    continue;
                };
                let Some(name) = left.strip_prefix("pub const") else {
                    continue;
                };
                let name = name.trim();
                let Some((name, ty)) = name.split_once(':') else {
                    continue;
                };
                if ty.trim() != "&str" {
                    continue;
                }
                let value = right.trim().trim_end_matches(';').trim();
                let value = value.trim_matches('"');
                if value.len() >= 3
                    && value.starts_with('V')
                    && value[1..].bytes().all(|b| b.is_ascii_digit())
                {
                    out.insert(name.trim().to_string(), value.to_string());
                }
            }
        }
        out
    }

    /// The severity-bearing code arguments the sources pass: the first
    /// argument of an `.error(` or `.warn(` call, the second of an
    /// `.error_edges(` or `.warn_edges(` call, and the `code` member of an
    /// `Issue {` literal. An argument that is neither a literal nor a known
    /// constant name fails through the returned reason.
    fn emitted_codes(
        sources: &[(String, String)],
        constants: &std::collections::BTreeMap<String, String>,
    ) -> Vec<(String, String, Result<String, String>)> {
        let mut out = Vec::new();
        for (file, text) in sources {
            for (call, arg_index) in [
                (".error(", 1usize),
                (".warn(", 1usize),
                (".error_edges(", 2usize),
                (".warn_edges(", 2usize),
                ("Issue::new(", 1usize),
            ] {
                let mut from = 0;
                while let Some(at) = text[from..].find(call) {
                    let at = from + at + call.len();
                    // The argument at `arg_index`, read as a balanced token.
                    let mut depth = 0usize;
                    let mut args = 0usize;
                    let mut token = String::new();
                    let mut i = at;
                    let bytes = text.as_bytes();
                    while i < bytes.len() {
                        let b = bytes[i] as char;
                        match b {
                            '(' | '[' | '{' => depth += 1,
                            ')' if depth == 0 => break,
                            ')' | ']' | '}' => depth -= 1,
                            ',' if depth == 0 => {
                                args += 1;
                                if args == arg_index {
                                    break;
                                } else {
                                    token.clear();
                                }
                            }
                            _ if args == arg_index - 1 => token.push(b),
                            _ => {}
                        }
                        i += 1;
                    }
                    let token = token.trim();
                    if !token.is_empty() {
                        let name = file.clone();
                        let what = format!("{call} in {name}");
                        // The bare identifier `code` is a forwarding
                        // parameter (a helper that passes its caller's
                        // argument on): the caller's literal is what the
                        // literal check above registers.
                        let bare = token.rsplit("::").next().unwrap_or(token);
                        if token == "code" {
                            out.push((what, token.to_string(), Ok(String::new())));
                        } else if let Some(v) = constants.get(token).or_else(|| constants.get(bare))
                        {
                            out.push((what, token.to_string(), Ok(v.clone())));
                        } else if token.starts_with('"') && token.ends_with('"') {
                            out.push((
                                what,
                                token.to_string(),
                                Ok(token.trim_matches('"').to_string()),
                            ));
                        } else {
                            out.push((what, token.to_string(), Err(token.to_string())));
                        }
                    }
                    from = at;
                }
            }
            // `Issue {` literals with a `code: <expr>` member, and
            // `Issue::new(` calls with the code as their first argument.
            let mut from = 0;
            while let Some(found) = text[from..].find("code:") {
                let at = from + found;
                let before = text[..at].trim_end();
                let is_issue_literal = before.ends_with('{')
                    && before[..before.len() - 1]
                        .trim()
                        .rsplit(char::is_whitespace)
                        .next()
                        .is_some_and(|word| word == "Issue" || word.ends_with("::Issue"));
                if !is_issue_literal {
                    from = at + 5;
                    continue;
                }
                let rest = text[at + 5..].trim_start();
                let token = rest.split(',').next().unwrap_or("").trim();
                if !token.is_empty() {
                    let what = format!("Issue {{ in {}", file);
                    let bare = token.rsplit("::").next().unwrap_or(token);
                    if token == "code" {
                        out.push((what, token.to_string(), Ok(String::new())));
                    } else if let Some(v) = constants.get(token).or_else(|| constants.get(bare)) {
                        out.push((what, token.to_string(), Ok(v.clone())));
                    } else if token.starts_with('"') && token.ends_with('"') {
                        out.push((
                            what,
                            token.to_string(),
                            Ok(token.trim_matches('"').to_string()),
                        ));
                    } else {
                        out.push((what, token.to_string(), Err(token.to_string())));
                    }
                }
                from = at + 5;
            }
        }
        out
    }

    fn is_code(s: &str) -> bool {
        s.len() >= 3 && s.starts_with('V') && s[1..].bytes().all(|b| b.is_ascii_digit())
    }

    fn number(code: &str) -> u32 {
        code[1..].parse().expect("a V code holds a number")
    }

    /// The one registry check: what the sources name is what the tables
    /// hold, and the tables are tidy.
    #[test]
    fn the_registry_is_what_the_validator_can_emit() {
        let sources = production_sources();
        let constants = code_constants(&sources);
        let literals = literal_codes(&sources);

        // 1. Every literal is registered; every active code is named once.
        for code in &literals {
            assert!(
                CODES.iter().any(|(c, _, _)| c == code)
                    || RETIRED_CODES.iter().any(|(c, _)| c == code),
                "the sources name {code}, which is in neither CODES nor RETIRED_CODES"
            );
        }
        for (code, _, _) in CODES {
            assert!(
                literals.contains(*code),
                "the active code {code} appears in no source literal"
            );
        }

        // 2. Every emitted code argument is a literal or a known constant.
        let emitted = emitted_codes(&sources, &constants);
        for (site, token, resolved) in emitted {
            match resolved {
                // An empty value is the forwarded `code` parameter.
                Ok(code) if code.is_empty() => {}
                Ok(code) => assert!(
                    is_code(&code),
                    "{site} passes {token}, which resolves to {code:?}: not a code"
                ),
                Err(token) => panic!(
                    "{site} passes {token}: not a literal and not a constant whose value is a \"V<n>\" string; make it one or teach the registry test about it"
                ),
            }
        }

        // 3. The tables are disjoint and sorted.
        for (code, _) in RETIRED_CODES {
            assert!(
                !CODES.iter().any(|(c, _, _)| c == code),
                "{code} is in both CODES and RETIRED_CODES"
            );
        }
        let check_sorted = |codes: &[u32], what: &str| {
            for w in codes.windows(2) {
                assert!(
                    w[0] < w[1],
                    "{what} is not sorted by number: {} then {}",
                    w[0],
                    w[1]
                );
            }
        };
        check_sorted(
            &CODES.iter().map(|(c, _, _)| number(c)).collect::<Vec<_>>(),
            "CODES",
        );
        check_sorted(
            &RETIRED_CODES
                .iter()
                .map(|(c, _)| number(c))
                .collect::<Vec<_>>(),
            "RETIRED_CODES",
        );
        assert_eq!(
            CODES.len(),
            CODES
                .iter()
                .map(|(c, _, _)| *c)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            "CODES holds a repeat"
        );

        // Every severity list holds at least one severity and no repeat.
        for (code, severities, _) in CODES {
            assert!(!severities.is_empty(), "{code} lists no severity");
            for (i, s) in severities.iter().enumerate() {
                assert!(
                    !severities[..i].contains(s),
                    "{code} repeats the severity {}",
                    severity_word(*s)
                );
            }
        }
    }
}
