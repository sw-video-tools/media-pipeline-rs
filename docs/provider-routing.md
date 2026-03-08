# Provider Routing

The `provider-router` crate should centralize all routing decisions.

Example policies:

- cheap local model for classification, chunk ranking, metadata extraction
- stronger remote model for planning, script refinement, final quality review
- dedicated TTS provider for narration
- dedicated image/video providers for scene visuals
- ASR provider for narration validation

No service should directly know provider-specific SDK details.
