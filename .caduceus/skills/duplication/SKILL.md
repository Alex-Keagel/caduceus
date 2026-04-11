---
name: duplication
version: "1.0"
description: Duplication workflow for code-quality tasks with quality and risk controls
categories: [code-quality, caduceus]
triggers: ["remove duplication", "deduplicate code"]
tools: [read_file, grep_search, edit_file]
---

# Duplication Skill Playbook

1. Clarify the objective for Duplication and restate expected outcomes, risks, and scope boundaries.
2. Inspect repository structure, ownership boundaries, and related modules before changing implementation details.
3. Identify constraints from existing conventions, architecture decisions, lint rules, and test requirements.
4. Create a short execution plan that includes analysis, implementation, validation, and documentation updates.
5. Prioritize high-impact issues first and explicitly note assumptions where requirements are ambiguous.
6. Use evidence from code search and existing patterns rather than introducing novel patterns without justification.
7. Apply small, reviewable edits and keep refactors behavior-preserving unless a requirement says otherwise.
8. Add or update automated checks relevant to the change to reduce future regressions in this area.
9. Verify edge cases, error paths, and rollback behavior where applicable to the selected workflow.
10. Check compatibility concerns such as runtime versions, dependency constraints, and integration contracts.
11. Evaluate maintainability: readability, complexity, cohesion, coupling, and long-term ownership burden.
12. Include security and privacy checks proportional to data sensitivity and external attack surface.
13. Compare before/after behavior with concrete examples, command output, or test evidence.
14. Capture tradeoffs made, including what was deferred and why it was not addressed in this pass.
15. Ensure naming and structure communicate intent clearly for future contributors and code reviewers.
16. Confirm generated artifacts or configs are deterministic and can be reproduced in CI/CD.
17. Validate developer ergonomics, including local setup friction and debugging clarity after changes.
18. Update user-facing documentation if behavior, setup, or interfaces have changed.
19. Provide a concise summary listing modified files, rationale, validation steps, and residual risks.
20. Close with follow-up recommendations specific to Duplication in the code-quality category.
21. If blockers appear, report them with actionable alternatives rather than stopping at failure messages.
22. Maintain strict focus on this skill domain while coordinating with adjacent skills only when necessary.
