# Nagi

Nagi is a local-first controller that turns Linear issues into work for AI coding agents and writes the results back to Linear. It exists so that the state of every piece of agent work can be read from Linear alone.

## Language

### Loop model

**Loop**:
One configured transition of an issue: which issues it may take, how it locks them, which agent profile runs, and which outcomes it may write back. One loop file is one loop.
_Avoid_: workflow, pipeline, phase, automation

**Tick**:
One pass of the controller over all loops. A tick first reconciles open attempts, then dispatches new ones by fetching candidate issues and claiming them.
_Avoid_: poll, iteration, cycle

**Tickets**:
The condition a loop uses to select candidate issues, expressed as Linear `IssueFilter` predicates plus fixed conditions the controller adds.
_Avoid_: query, trigger, selector

### Claiming

**Entry label**:
The label in a loop's tickets that belongs to the same label group as its lock label. Claiming swaps the entry label for the lock label.
_Avoid_: trigger label, input label

**Lock label**:
The label that marks an issue as claimed by one loop.
_Avoid_: in-progress label, claim label, working label

**Human label**:
A configured label that only a person moves an issue out of. Nagi may add one to hand work over but never removes it.
_Avoid_: review label, manual label

**Attention label**:
A configured label outside every loop's label group that Nagi adds when an attempt fails and needs a person.
_Avoid_: error label, blocked label

**Claim**:
Taking one issue for one loop: a local intent row plus the label swap on Linear, confirmed by reading the issue again.
_Avoid_: lock, lease, checkout

**Attempt**:
One claim's lifetime, from the intent row to done, failed, or lost.
_Avoid_: run, session, job, task

**Lost**:
The end of an attempt whose lock label turns out to be missing because a person or another writer changed the issue.
_Avoid_: contested, conflict

**Rework**:
A person returning an issue to the entry label, after removing the attention label if it has one, so that a new attempt starts.
_Avoid_: retry, resume, send back

### Agents and outcomes

**Instruction**:
The prompt template a loop renders for its agent. The operator writes it; issue text is embedded as untrusted data.
_Avoid_: prompt, system prompt, task description

**Outcome**:
One allowed end state of a loop, with the labels and state it applies and the requirements Nagi checks before applying them.
_Avoid_: result, transition, verdict

**Decision**:
The outcome name an agent chooses in its report.
_Avoid_: verdict, choice, classification

**Report**:
The structured file an agent writes at the end of an attempt. Nagi validates it and treats it as input, not proof.
_Avoid_: result, output, summary

### Runtime

**Runtime**:
The component that starts, prompts, observes, interrupts, and stops an agent for an attempt. Herdr is the only runtime today.
_Avoid_: backend, adapter, driver, provider

**Agent profile**:
A named choice of vendor CLI that loops refer to by name.
_Avoid_: backend, executor, agent type

**Workspace**:
The Herdr workspace and the directory an attempt's agent works in: a shared scratch directory, or a Git worktree of one repository.
_Avoid_: environment, sandbox, target

**Workspace label**:
A label in the group a loop reads its agent profile from. Each label's name is an agent profile name.
_Avoid_: environment label, routing label, runtime label

**Sub-issue**:
A Linear sub-issue that carries one workspace's share of a larger issue. Every loop treats it as an ordinary issue.
_Avoid_: subtask, split, fan-out
