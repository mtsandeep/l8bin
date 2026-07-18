-- Legacy docker-access grants claimed project isolation that the proxy could not enforce.
-- Require users to explicitly grant the new read-only docker-observe capability.
DELETE FROM project_capabilities WHERE capability = 'docker-access';
UPDATE projects SET allow_docker_access = 0;
