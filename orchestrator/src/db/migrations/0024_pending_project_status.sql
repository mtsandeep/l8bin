-- Separate newly created projects from staged projects awaiting runtime configuration.
-- A staged single-service project has an image; a staged compose project has deploy_type.
UPDATE projects
SET status = 'pending'
WHERE status = 'unconfigured'
  AND (image IS NULL OR trim(image) = '')
  AND deploy_type IS NULL;
