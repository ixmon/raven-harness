# AutoHarness: Improving LLM Agents by Automatically Synthesizing a Code Harness

**Paper**: [arXiv:2603.03329](https://arxiv.org/abs/2603.03329)  
**Authors**: Xinghua Lou, Miguel Lázaro-Gredilla, Antoine Dedieu, Carter Wendelken, Wolfgang Lehrach, Kevin P. Murphy  
**Published**: February 10, 2026  
**Institution**: Google DeepMind

## Core Idea

AutoHarness addresses a fundamental problem in LLM agents: when used as agents, LLMs often try to perform actions that are not just suboptimal, but are strictly prohibited by the external environment. For example, in the Kaggle GameArena chess competition, **78% of Gemini-2.5-Flash losses were attributed to illegal moves**.

Instead of fine-tuning the LLM to produce fewer illegal actions (which is expensive and degrades other capabilities), AutoHarness uses the LLM to **synthesize its own constraint checker** - a code harness that prevents such failures.

## Key Approach

The AutoHarness technique works through iterative code refinement:

1. **Initial harness synthesis**: LLM generates a code harness around itself
2. **Feedback from environment**: The harness is tested in the environment
3. **Iterative refinement**: Based on failures, the LLM refines the harness code
4. **Result**: A working harness that prevents illegal/prohibited actions

### Two-Stage Process

**Stage 1: Harness Synthesis**
- LLM synthesizes a constraint checker/harness
- The harness prevents illegal moves/actions
- Enables smaller models to outperform larger ones

**Stage 2: Full Code-Policy Generation** (pushing to the limit)
- LLM generates the entire policy in code
- Eliminates the need to use the LLM at decision-making time
- The resulting code-policy receives higher average reward than larger models

## Results

### TextArena Games
- Prevents all illegal moves in **145 different TextArena games** (both 1-player and 2-player)
- Gemini-2.5-Flash with AutoHarness outperforms Gemini-2.5-Pro
- Code-policy outperforms Gemini-2.5-Pro and GPT-5.2-High on 16 TextArena 1-player games

### Key Insights
- Using a smaller model to synthesize a custom code harness can outperform much larger models
- More cost-effective than fine-tuning larger models
- The harness (not just the policy) is transferable

## Why This Matters

AutoHarness demonstrates a **stark inversion** of the typical approach:

| Traditional Approach | AutoHarness Approach |
|---------------------|---------------------|
| Fine-tune LLM to be better | Synthesize harness around LLM |
| Improve LLM capabilities | Improve environment compliance |
| Expensive, degrades other capabilities | Cheaper, preserves capabilities |

## Connection to Other Techniques

AutoHarness aligns with and extends concepts from other papers:

- **LLM-as-Code**: Both treat code as the substrate for agent infrastructure
- **Code as Agent Harness**: AutoHarness is an example of harness interface + mechanisms
- **AHE**: AutoHarness uses iterative refinement with feedback, similar to AHE's observability-driven evolution
- **Meta-Harness**: Both use filesystem access to track and improve harness variants

## Implementation Considerations

The AutoHarness approach requires:
1. A small number of rounds of iterative code refinement
2. Feedback from the environment (success/failure signals)
3. The ability to execute and test synthesized code
4. A representation of the harness that the LLM can generate and modify

## Implications for Raven Harness

AutoHarness suggests several enhancements:

1. **Self-synthesized harnesses**: The LLM could generate its own constraint checkers for specific tasks
2. **Environment feedback loops**: Use execution results to drive harness refinement
3. **Code-policy generation**: For repetitive tasks, generate code policies that don't require LLM at decision time
4. **Cost optimization**: Use smaller models with well-designed harnesses instead of larger models
