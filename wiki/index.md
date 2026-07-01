# Coding Harness Techniques for LLM Enhancement

## Key Research Papers

### 1. LLM-as-Code: Agentic Programming (arXiv:2606.15874)
**Core Idea**: The LLM should NOT be the orchestrator. Code should govern all control flow (loops, branching, sequencing), while the LLM acts as an "adaptive component" invoked only where reasoning or generation is needed.

**Key Techniques**:
- **Code-driven workflow**: Program owns control flow, LLM only called for reasoning/generation
- **DAG-structured context**: Context bounded by call depth, not flat history accumulation
- **Multi-agent collaboration**: Agents as functions over the DAG graph
- **Self-programmed evolution**: Agent improvements committed as durable code

**Why Orchestrator Fails**:
- Deterministic vs. Probabilistic mismatch: Control flow requires exact execution, but LLM samples from distributions
- Unguaranteed compliance: No native means to obey constraints (e.g., check before write)
- Context overflow: History grows unboundedly

### 2. Code as Agent Harness (arXiv:2605.18747)
**Core Idea**: Code is no longer just a target output—it serves as the operational substrate for agent infrastructure.

**Three-Layer Framework**:

| Layer | Components | Purpose |
|-------|-----------|---------|
| **Harness Interface** | Reasoning, Action, Environment Modeling | Connect agents to external systems |
| **Harness Mechanisms** | Planning, Memory, Tool Use, Feedback-driven Control | Long-horizon execution with reliability |
| **Multi-Agent Scaling** | Shared code artifacts, Coordination, Review, Verification | Collaborative agent systems |

**Applications**: Coding assistants, GUI/OS automation, embodied agents, scientific discovery, DevOps, enterprise workflows

**Open Challenges**:
- Evaluation beyond final task success
- Verification under incomplete feedback
- Regression-free harness improvement
- Consistent shared state across multiple agents
- Human oversight for safety-critical actions
- Multimodal environment extensions

### 3. Agentic Harness Engineering (AHE) (arXiv:2604.25850)
**Core Idea**: Automatic evolution of harnesses through observability-driven closed loops.

**Three Observability Pillars**:

1. **Component observability**: Every editable harness component has file-level representation → explicit and revertible action space
2. **Experience observability**: Distill millions of raw trajectory tokens into layered, drill-down evidence corpus
3. **Decision observability**: Pair every edit with a self-declared prediction, verified against outcomes

**Results**:
- 10 AHE iterations lift pass@1 on Terminal-Bench 2 from 69.7% to 77.0%
- Surpasses human-designed Codex-CLI (71.9%)
- Frozen harness transfers without re-evolution

**Key Insight**: Tools, middleware, and long-term memory transfer; prose-level strategy does not.

### 4. Meta-Harness (arXiv:2603.28052)
**Core Idea**: End-to-end optimization of model harnesses via an outer-loop system that searches over harness code.

**Approach**:
- Agentic proposer accesses source code, scores, and execution traces of all prior candidates
- Uses filesystem to track and compare harness variants

**Results**:
- +7.7 points on online text classification (4x fewer context tokens)
- +4.7 points on 200 IMO-level math problems
- Surpasses best hand-engineered baselines on TerminalBench-2

### 5. AutoHarness (arXiv:2603.03329)
**Core Idea**: "Code as harness" framework where the LLM itself completes the agent by coding its own harness.

See [AutoHarness Details](./autoharness.md) for a comprehensive overview of the technique, results, and implications.

### 6. VeriGuard (arXiv:2510.05156)
**Core Idea**: Formal safety guarantees for LLM agents through a dual-stage architecture with offline validation and online monitoring.

See [VeriGuard Details](./veriguard.md) for a comprehensive overview of the technique and its implications.

### 7. Test Document
A test markdown page demonstrating various markdown features including headers, code blocks, lists, tables, and task lists.

See [Test Document](./test.md) for details.

## Implications for Raven Harness

### Current Raven Features (Aligned with Research)
- **Persistent sessions** → Memory mechanism
- **File summary cache** → Experience observability / context management
- **Judge/Super Judge** → Feedback-driven control
- **Execution approval modes** → Safety/observability
- **Context budget probing** → Token-aware optimization
- **Agent capabilities** → See [agent-capabilities.md](./agent-capabilities.md) for detailed agent architecture documentation
- **Agent capabilities documentation** → Implementation details and patterns

See [TUI Agent Capabilities](./research/agent-capabilities.md) for implementation details on the agent loop, turn metrics, session persistence, and integration with AutoHarness/VeriGuard concepts.

### Potential Enhancements from Research

1. **DAG-structured context** (LLM-as-Code)
   - Replace flat conversation history with call tree
   - Bound context by call depth, not step count

2. **Code-driven workflow** (LLM-as-Code)
   - Extract control flow from LLM prompts
   - Make looping/branching deterministic in Rust code

3. **Observability infrastructure** (AHE)
   - Track harness component changes
   - Record predictions and verify against outcomes

4. **Meta-optimization** (Meta-Harness)
   - Automated harness evolution
   - Track and compare harness variants

5. **Self-improving harness** (AutoHarness)
   - LLM codes its own harness improvements
   - Commit improvements as code

6. **Formal safety guarantees** (VeriGuard)
   - Add formal verification of agent actions against safety specifications
   - Implement lightweight online validation of proposed actions

## References
- LLM-as-Code: https://arxiv.org/abs/2606.15874
- Code as Agent Harness: https://arxiv.org/abs/2605.18747
- Agentic Harness Engineering: https://arxiv.org/abs/2604.25850
- Meta-Harness: https://arxiv.org/abs/2603.28052
- AutoHarness: https://arxiv.org/abs/2603.03329
- VeriGuard: https://arxiv.org/abs/2510.05156
