use super::*;

fn port(published: Option<u16>, target: u16) -> PortMapping {
    PortMapping { published, target }
}

#[test]
fn reads_image_build_and_short_ports_with_comments_and_quotes() {
    let text = r#"# header
services:
  # a comment
  postgres:
    image: postgres:16-alpine
    ports:
      - "5433:5432"   # host:container
    environment:
      POSTGRES_DB: x
  "laravel":
    build:
      context: ./acme-billing
      dockerfile: Dockerfile
  api:
    build: .
    ports: ["3001:3001", '127.0.0.1:9000:9000/tcp', "8080"]
volumes:
  data:
"#;
    let services = parse_compose(text).expect("has services");
    assert_eq!(services.len(), 3);
    assert_eq!(services[0].name, "postgres");
    assert_eq!(services[0].image.as_deref(), Some("postgres:16-alpine"));
    assert_eq!(services[0].build, None);
    assert_eq!(services[0].ports, vec![port(Some(5433), 5432)]);
    assert_eq!(services[1].name, "laravel");
    assert_eq!(services[1].image, None);
    assert_eq!(services[1].build.as_deref(), Some("./acme-billing"));
    assert_eq!(services[2].build.as_deref(), Some("."));
    assert_eq!(
        services[2].ports,
        vec![
            port(Some(3001), 3001),
            port(Some(9000), 9000),
            port(None, 8080)
        ]
    );
    assert!(services.iter().all(|s| s.unreadable.is_empty()));
}

#[test]
fn reads_long_port_syntax_and_four_space_indentation() {
    let text = "services:\n    web:\n        image: nginx:alpine\n        ports:\n            - target: 80\n              published: \"8500\"\n              protocol: tcp\n            - published: 443\n              target: 443\n";
    let services = parse_compose(text).expect("has services");
    assert_eq!(services.len(), 1);
    assert_eq!(
        services[0].ports,
        vec![port(Some(8500), 80), port(Some(443), 443)]
    );
}

#[test]
fn an_interpolated_port_is_dropped_with_a_note() {
    let text = "services:\n  nginx:\n    image: nginx\n    ports:\n      - \"${APP_PORT:-8000}:80\"\n      - \"443:443\"\n";
    let services = parse_compose(text).expect("has services");
    assert_eq!(services[0].ports, vec![port(Some(443), 443)]);
    assert_eq!(
        services[0].unreadable,
        vec!["port \"${APP_PORT:-8000}:80\" is not readable".to_string()]
    );
}

#[test]
fn a_file_without_services_returns_none() {
    assert_eq!(parse_compose("version: '3'\nvolumes:\n  a:\n"), None);
    assert_eq!(parse_compose(""), None);
}

#[test]
fn image_names_are_reduced_to_their_last_segment() {
    assert_eq!(
        image_base("timescale/timescaledb:latest-pg16"),
        "timescaledb"
    );
    assert_eq!(image_base("postgres:16-alpine"), "postgres");
    assert_eq!(image_base("ghcr.io/org/Redis@sha256:abc"), "redis");
    assert_eq!(image_base("localhost:5000/minio/minio"), "minio");
    assert!(is_infra_image("axllent/mailpit"));
    assert!(is_infra_image("timescale/timescaledb:latest-pg16"));
    assert!(!is_infra_image("traccar/traccar:latest"));
    assert!(!is_infra_image("nginx:alpine"));
}

#[test]
fn compose_file_names_and_ignore_words() {
    assert!(is_compose_name("docker-compose.yml"));
    assert!(is_compose_name("docker-compose.dev.yaml"));
    assert!(is_compose_name("compose.yaml"));
    assert!(!is_compose_name("supabase-compose.yml"));
    assert!(!is_compose_name("docker-compose.json"));
    assert_eq!(ignore_word("docker-compose.dokploy.yml"), Some("dokploy"));
    assert_eq!(ignore_word("docker-compose.PROD.yml"), Some("prod"));
    assert_eq!(ignore_word("compose.production.yml"), Some("production"));
    assert_eq!(ignore_word("docker-compose.deploy.yml"), Some("deploy"));
    assert_eq!(ignore_word("docker-compose.staging.yml"), Some("staging"));
    assert_eq!(ignore_word("docker-compose.supabase.yml"), None);
}

#[test]
fn reads_depends_on_in_list_and_mapping_forms() {
    let text = "services:\n  traccar:\n    image: traccar/traccar:latest\n    depends_on:\n      traccar-db:\n        condition: service_healthy\n      \"redis\":\n        condition: service_started\n    ports:\n      - \"8082:8082\"\n  nginx:\n    image: nginx:alpine\n    depends_on:\n      - laravel\n  web:\n    image: x\n    depends_on: [api, \"db\"]\n";
    let services = parse_compose(text).expect("has services");
    assert_eq!(services[0].depends_on, vec!["traccar-db", "redis"]);
    assert_eq!(services[0].ports, vec![port(Some(8082), 8082)]);
    assert_eq!(services[1].depends_on, vec!["laravel"]);
    assert_eq!(services[2].depends_on, vec!["api", "db"]);
}
