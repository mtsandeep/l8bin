-- Safety net: cascade terminal/error states from projects to project_services
-- Catches any code path that bypasses the centralized status module.
-- Does NOT cascade "running" — that should only be set when containers are confirmed alive.
CREATE TRIGGER IF NOT EXISTS projects_status_sync_services
AFTER UPDATE OF status ON projects
FOR EACH ROW
WHEN OLD.status != NEW.status
  AND NEW.status IN ('stopped', 'stopping', 'error', 'deploying')
BEGIN
  UPDATE project_services SET status = NEW.status WHERE project_id = NEW.id;
END;
