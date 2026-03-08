# Artifact Schemas

Every artifact should be wrapped in a common envelope:

- artifact_id
- artifact_type
- version
- created_at
- produced_by
- input_refs
- evidence_refs
- validation
- content_hash
- payload

Recommended artifact types:

- IdeaBrief
- ProjectPlan
- ResearchPacket
- FactTable
- NarrationScript
- BeatSheet
- Storyboard
- ShotList
- VisualPromptSet
- CueSheet
- NarrationSegments
- TtsValidationReport
- CaptionTrack
- RenderManifest
- PreviewRender
- FinalRender
- QaReport
