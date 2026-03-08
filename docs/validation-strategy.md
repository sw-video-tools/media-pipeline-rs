# Validation Strategy

## Principles

1. Prefer deterministic tools before LLMs.
2. Separate generator and validator roles.
3. Require evidence refs for factual claims.
4. Re-render only failing segments.
5. Cache working sets and artifact hashes.

## Example checks

- script claims backed by evidence refs
- TTS output ASR-matches source script within threshold
- captions align with narration timing
- loudness, silence, clipping, and black-frame checks
- final render duration inside target bounds
