use std::collections::HashMap;

use crate::error::{ComposeError, Result};
use crate::parse::ComposeFile;

impl ComposeFile {
    /// Return service names in topological order (dependencies first).
    /// Uses Kahn's algorithm.
    pub fn topological_sort(&self) -> Result<Vec<String>> {
        if self.services.is_empty() {
            return Err(ComposeError::NoServices);
        }

        // Build adjacency list and in-degree count using owned strings
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        let mut dependents: HashMap<String, Vec<String>> = HashMap::new();

        for name in self.services.keys() {
            in_degree.entry(name.clone()).or_insert(0);
        }

        for (name, service) in &self.services {
            for dep in service.dependency_names() {
                // Validate ghost dependency
                if !self.services.contains_key(&dep) {
                    return Err(ComposeError::GhostDependency {
                        service: name.clone(),
                        dep,
                    });
                }
                in_degree.entry(dep.clone()).or_insert(0);
                *in_degree.entry(name.clone()).or_insert(0) += 1;
                dependents
                    .entry(dep)
                    .or_default()
                    .push(name.clone());
            }
        }

        // Kahn's algorithm
        let mut queue: Vec<String> = in_degree
            .iter()
            .filter(|&(_, &deg)| deg == 0)
            .map(|(name, _)| name.clone())
            .collect();
        queue.sort();

        let mut order = Vec::new();
        while let Some(current) = queue.pop() {
            order.push(current.clone());
            if let Some(deps) = dependents.get(&current) {
                for dep in deps {
                    let deg = in_degree.get_mut(dep).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push(dep.clone());
                        queue.sort();
                    }
                }
            }
        }

        if order.len() != self.services.len() {
            let remaining: Vec<String> = in_degree
                .keys()
                .filter(|n| !order.contains(n))
                .cloned()
                .collect();
            return Err(ComposeError::CycleDetected {
                chain: remaining.join(" -> "),
            });
        }

        Ok(order)
    }

    /// Detect if there's a dependency cycle. Returns None if no cycle.
    pub fn detect_cycles(&self) -> Option<Vec<String>> {
        match self.topological_sort() {
            Ok(_) => None,
            Err(ComposeError::CycleDetected { chain }) => Some(
                chain
                    .split(" -> ")
                    .map(|s| s.to_string())
                    .collect(),
            ),
            _ => None,
        }
    }

    /// Validate that all depends_on references point to existing services.
    /// Returns a list of ghost dependencies (service, missing_dep) pairs.
    pub fn validate_ghost_deps(&self) -> Vec<(&str, String)> {
        let mut ghosts = Vec::new();
        for (name, service) in &self.services {
            for dep in service.dependency_names() {
                if !self.services.contains_key(&dep) {
                    ghosts.push((name.as_str(), dep));
                }
            }
        }
        ghosts
    }

    /// Detect the public service. Priority:
    /// 1. Service with label `litebin.public=true`
    /// 2. Service exposing port 80 or 443
    /// 3. Service exposing any port (first one with a port)
    /// 4. None (error)
    pub fn detect_public_service(&self) -> Result<Option<String>> {
        // Priority 1: explicit label
        let labeled: Vec<&String> = self
            .services
            .iter()
            .filter(|(_, svc)| svc.is_public_by_label())
            .map(|(name, _)| name)
            .collect();
        if labeled.len() > 1 {
            return Err(ComposeError::MultiplePublicServices {
                services: labeled.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
            });
        }
        if let Some(name) = labeled.first() {
            return Ok(Some((*name).clone()));
        }

        // Priority 2: port 80 or 443
        let well_known: Vec<&String> = self
            .services
            .iter()
            .filter(|(_, svc)| {
                svc.exposed_ports()
                    .iter()
                    .any(|(port, _)| *port == 80 || *port == 443)
            })
            .map(|(name, _)| name)
            .collect();
        if well_known.len() == 1 {
            return Ok(Some((*well_known[0]).clone()));
        }

        // Priority 3: any service with a port
        let with_port: Vec<&String> = self
            .services
            .iter()
            .filter(|(_, svc)| !svc.exposed_ports().is_empty())
            .map(|(name, _)| name)
            .collect();
        if with_port.len() == 1 {
            return Ok(Some((*with_port[0]).clone()));
        }

        // No public service found (might be intentional for internal-only projects)
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prop_assert_eq;
    use crate::ComposeParser;

    #[test]
    fn topological_sort_linear() {
        let yaml = r#"
services:
  db:
    image: postgres
  web:
    image: nginx
    depends_on:
      - db
"#;
        let compose = ComposeParser::parse(yaml).unwrap();
        let order = compose.topological_sort().unwrap();
        assert_eq!(order[0], "db");
        assert_eq!(order[1], "web");
    }

    #[test]
    fn topological_sort_diamond() {
        let yaml = r#"
services:
  db:
    image: postgres
  cache:
    image: redis
  api:
    image: node
    depends_on: [db, cache]
  web:
    image: nginx
    depends_on: [api]
"#;
        let compose = ComposeParser::parse(yaml).unwrap();
        let order = compose.topological_sort().unwrap();
        let db_idx = order.iter().position(|s| s == "db").unwrap();
        let cache_idx = order.iter().position(|s| s == "cache").unwrap();
        let api_idx = order.iter().position(|s| s == "api").unwrap();
        let web_idx = order.iter().position(|s| s == "web").unwrap();
        assert!(db_idx < api_idx);
        assert!(cache_idx < api_idx);
        assert!(api_idx < web_idx);
    }

    #[test]
    fn detect_cycle() {
        let yaml = r#"
services:
  a:
    image: img
    depends_on: [b]
  b:
    image: img
    depends_on: [a]
"#;
        let compose = ComposeParser::parse(yaml).unwrap();
        assert!(compose.detect_cycles().is_some());
        assert!(compose.topological_sort().is_err());
    }

    #[test]
    fn detect_ghost_dep() {
        let yaml = r#"
services:
  web:
    image: nginx
    depends_on: [nonexistent]
"#;
        let compose = ComposeParser::parse(yaml).unwrap();
        let ghosts = compose.validate_ghost_deps();
        assert_eq!(ghosts.len(), 1);
        assert_eq!(ghosts[0].0, "web");
        assert_eq!(ghosts[0].1, "nonexistent");
    }

    #[test]
    fn detect_public_by_label() {
        let yaml = r#"
services:
  db:
    image: postgres
  web:
    image: nginx
    labels:
      litebin.public: "true"
"#;
        let compose = ComposeParser::parse(yaml).unwrap();
        assert_eq!(
            compose.detect_public_service().unwrap(),
            Some("web".to_string())
        );
    }

    #[test]
    fn detect_public_by_port_80() {
        let yaml = r#"
services:
  db:
    image: postgres
  web:
    image: nginx
    ports:
      - "80:8080"
"#;
        let compose = ComposeParser::parse(yaml).unwrap();
        assert_eq!(
            compose.detect_public_service().unwrap(),
            Some("web".to_string())
        );
    }

    #[test]
    fn detect_public_by_any_port() {
        let yaml = r#"
services:
  web:
    image: nginx
    ports:
      - "3000"
"#;
        let compose = ComposeParser::parse(yaml).unwrap();
        assert_eq!(
            compose.detect_public_service().unwrap(),
            Some("web".to_string())
        );
    }

    #[test]
    fn no_public_service() {
        let yaml = r#"
services:
  db:
    image: postgres
"#;
        let compose = ComposeParser::parse(yaml).unwrap();
        assert_eq!(compose.detect_public_service().unwrap(), None);
    }

    #[test]
    fn multiple_public_labels_error() {
        let yaml = r#"
services:
  api:
    image: node
    labels:
      litebin.public: "true"
  worker:
    image: node
    labels:
      litebin.public: "true"
"#;
        let compose = ComposeParser::parse(yaml).unwrap();
        assert!(compose.detect_public_service().is_err());
    }

    #[test]
    fn depends_on_map_format() {
        let yaml = r#"
services:
  db:
    image: postgres
  web:
    image: nginx
    depends_on:
      db:
        condition: service_started
"#;
        let compose = ComposeParser::parse(yaml).unwrap();
        let order = compose.topological_sort().unwrap();
        assert_eq!(order[0], "db");
    }

    proptest::proptest! {
        #[test]
        fn prop_single_service_always_first(
            svc_name in "[a-z][a-z0-9]{2,8}"
        ) {
            let yaml = format!(r#"
services:
  {}:
    image: test
"#, svc_name);
            let compose = ComposeParser::parse(&yaml).unwrap();
            let order = compose.topological_sort().unwrap();
            prop_assert_eq!(order.len(), 1);
            prop_assert_eq!(&order[0], &svc_name);
        }
    }
}
