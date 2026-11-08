-- Long-lived demo instances: the operator's edit copy and (shared mode) the
-- visitor-facing copy. No FK — instances are deleted through normal project
-- teardown, which the demo disable path runs first.
ALTER TABLE demo_config ADD COLUMN edit_instance_id TEXT;
ALTER TABLE demo_config ADD COLUMN shared_instance_id TEXT;
