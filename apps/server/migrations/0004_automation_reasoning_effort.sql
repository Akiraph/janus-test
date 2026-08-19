-- Automation execution model thinking level. Defaults to 'high' so new
-- Automation sessions request high reasoning effort for their first turn
-- unless the owner overrides it. Values match ReasoningEffort
-- (none|low|medium|high|xhigh|max).
ALTER TABLE automation_settings ADD COLUMN reasoning_effort TEXT NOT NULL DEFAULT 'high';
