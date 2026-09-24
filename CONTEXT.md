# Nagi

Nagi is a local-first controller that turns Linear issues into work for AI coding agents and writes the results back to Linear. It exists so that the state of every piece of agent work can be read from Linear alone.

## Language

### Loop model

**Loop**:
One configured transition of an issue: which issues it may take, how it locks them, which agent profile runs, and which outcomes it may write back. One file under `loops/` is one loop.
_Avoid_: workflow, pipeline, phase, automation

**Tick**:
One pass of the controller over all loops. A tick first reconciles running attempts, then dispatches new ones.
_Avoid_: poll, iteration, cycle

**Reconcile**:
The first half of a tick, where the controller re-reads every claimed issue and observes its agent.

**Dispatch**:
The second half of a tick, where the controller fetches candidate issues for each loop and claims them.

**Tickets**:
The condition a loop uses to select candidate issues, expressed as Linear `IssueFilter` predicates plus fixed guards the controller adds.
_Avoid_: query, trigger, selector

### Claiming

**Entry label**:
The label in a loop's tickets that belongs to the same label group as its lock label. Claiming swaps the entry label for the lock label.
_Avoid_: trigger label, input label

**Lock label**:
The label that marks an issue as claimed by one loop. Only Nagi adds a lock label; humans remove it or restore the entry label.
_Avoid_: in-progress label, claim label, working label

**Human label**:
A label only a person moves an issue out of. Nagi may add one to hand work over but never removes it and never uses it as an entry or lock label.
_Avoid_: review label, manual label

**Attention label**:
A label outside every loop's label group that Nagi adds when an attempt needs a person. It is the signal that something failed, stalled, or could not be decided.
_Avoid_: error label, blocked label

**Claim**:
The act of taking one issue for one loop: a local intent row plus the label swap on Linear, verified by re-reading the issue.
_Avoid_: lock, lease, checkout

**Attempt**:
One claim's lifetime, from intent row to done, failed, or lost. An issue has at most one active attempt across all loops.
_Avoid_: run, session, job, task

**Lost**:
The terminal state of an attempt whose claim was found not to hold on re-read, because another writer moved the issue first. Nagi writes nothing to Linear for a lost attempt.
_Avoid_: contested, conflict

**Rework**:
A person returning an issue from a human label to an entry label so that a new attempt starts. With reuse enabled, the new attempt continues in the same workspace, and Nagi starts a new agent there if the previous one is gone.
_Avoid_: retry, resume, send back

### Agents and outcomes

**Instruction**:
The prompt template a loop renders for its agent. Trusted text written by the operator; the issue body is embedded as untrusted data.
_Avoid_: prompt, system prompt, task description

**Outcome**:
One allowed end state of a loop, named in `expected_state`, with the labels and state it applies and the requirements Nagi checks before applying them.
_Avoid_: result, transition, verdict

**Decision**:
The outcome name an agent chooses in its report. The agent knows outcome names, never label or state names.
_Avoid_: verdict, choice, classification

**Report**:
The structured document an agent writes at the end of an attempt: decision, selected labels, pull requests, summary, or a blocked reason. Nagi validates it with one parser and never trusts it as proof of completion.
_Avoid_: result, output, summary

### Runtime

**Runtime**:
The component that starts, prompts, observes, interrupts, and stops an agent for an attempt. Herdr is the only runtime today; the agent runs in a Herdr pane.
_Avoid_: backend, adapter, driver, provider

**Agent profile**:
A named choice of vendor CLI (and its extra arguments) that loops refer to by name. Workspace labels carry profile names.
_Avoid_: backend, executor, agent type

**Kind**:
The vendor CLI Herdr launches for an agent profile, such as codex, claude, or cursor.
_Avoid_: model, vendor, agent type

**Workspace**:
The Herdr workspace and the directory an attempt's agent works in: a shared scratch directory, or a Git worktree of one repository.
_Avoid_: environment, sandbox, target

**Workspace label**:
A label from the `workspace` group that names the agent profile that should implement an issue. Triage chooses it; the implement loop selects the profile from it.
_Avoid_: environment label, routing label, runtime label

**Child issue**:
A Linear sub-issue that carries one workspace's share of a parent issue's work. It is an ordinary ticket for every loop. Once its children exist, the parent stays in a human label, so no loop matches it.
_Avoid_: subtask, split, fan-out
