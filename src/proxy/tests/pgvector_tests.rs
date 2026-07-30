use super::*;

fn test_config() -> PgvectorConfig {
    PgvectorConfig {
        url: "postgresql://localhost:5432/test".to_string(),
        table: "documents".to_string(),
        embedding_column: "embedding".to_string(),
        content_column: "content".to_string(),
        title_column: Some("title".to_string()),
        metadata_columns: vec!["source".to_string(), "category".to_string()],
        distance_metric: DistanceMetric::Cosine,
        dimensions: Some(384),
        timeout_secs: 30,
    }
}

#[test]
fn test_distance_metric_operators() {
    assert_eq!(DistanceMetric::Cosine.operator(), "<=>");
    assert_eq!(DistanceMetric::L2.operator(), "<->");
    assert_eq!(DistanceMetric::InnerProduct.operator(), "<#>");
}

#[test]
fn test_distance_metric_similarity_cosine() {
    let m = DistanceMetric::Cosine;
    assert!((m.to_similarity(0.0) - 1.0).abs() < 0.001);
    assert!((m.to_similarity(0.5) - 0.5).abs() < 0.001);
    assert!((m.to_similarity(1.0) - 0.0).abs() < 0.001);
}

#[test]
fn test_distance_metric_similarity_l2() {
    let m = DistanceMetric::L2;
    assert!((m.to_similarity(0.0) - 1.0).abs() < 0.001);
    assert!((m.to_similarity(1.0) - 0.5).abs() < 0.001);
}

#[test]
fn test_distance_metric_similarity_inner_product() {
    let m = DistanceMetric::InnerProduct;
    assert!((m.to_similarity(-0.9) - 0.9).abs() < 0.001);
    assert!((m.to_similarity(-0.5) - 0.5).abs() < 0.001);
}

#[test]
fn test_format_vector() {
    assert_eq!(format_vector(&[0.1, 0.2, 0.3]), "[0.1,0.2,0.3]");
    assert_eq!(format_vector(&[]), "[]");
    assert_eq!(format_vector(&[1.0]), "[1]");
}

#[test]
fn test_build_query_sql_cosine() {
    let config = test_config();
    let sql = build_query_sql(&config);
    assert!(sql.contains("SELECT"));
    assert!(sql.contains("content"));
    assert!(sql.contains("title"));
    assert!(sql.contains("<=>"));
    assert!(sql.contains("ORDER BY embedding <=> $1::vector"));
    assert!(sql.contains("LIMIT $2"));
}

#[test]
fn test_build_query_sql_l2() {
    let mut config = test_config();
    config.distance_metric = DistanceMetric::L2;
    let sql = build_query_sql(&config);
    assert!(sql.contains("<->"));
}

#[test]
fn test_build_query_sql_inner_product() {
    let mut config = test_config();
    config.distance_metric = DistanceMetric::InnerProduct;
    let sql = build_query_sql(&config);
    assert!(sql.contains("<#>"));
}

#[test]
fn test_build_select_columns_minimal() {
    let config = PgvectorConfig {
        url: "postgresql://localhost/db".to_string(),
        table: "docs".to_string(),
        embedding_column: "vec".to_string(),
        content_column: "body".to_string(),
        title_column: None,
        metadata_columns: vec![],
        distance_metric: DistanceMetric::Cosine,
        dimensions: None,
        timeout_secs: 30,
    };
    let cols = build_select_columns(&config);
    assert_eq!(cols, "body, vec <=> $1::vector AS distance");
}

#[test]
fn test_build_select_columns_full() {
    let config = test_config();
    let cols = build_select_columns(&config);
    assert!(cols.contains("content"));
    assert!(cols.contains("title"));
    assert!(cols.contains("source"));
    assert!(cols.contains("category"));
    assert!(cols.contains("AS distance"));
}

#[test]
fn test_config_validate_ok() {
    let config = test_config();
    assert!(config.validate().is_ok());
}

#[test]
fn test_config_validate_empty_url() {
    let mut config = test_config();
    config.url = String::new();
    assert!(config.validate().unwrap_err().contains("url"));
}

#[test]
fn test_config_validate_empty_table() {
    let mut config = test_config();
    config.table = String::new();
    assert!(config.validate().unwrap_err().contains("table"));
}

#[test]
fn test_config_validate_unsafe_identifier() {
    let mut config = test_config();
    config.table = "docs; DROP TABLE--".to_string();
    assert!(config.validate().unwrap_err().contains("unsafe"));
}

#[test]
fn test_safe_identifier() {
    assert!(is_safe_identifier("documents"));
    assert!(is_safe_identifier("my_table_123"));
    assert!(is_safe_identifier("col_A"));
    assert!(!is_safe_identifier(""));
    assert!(!is_safe_identifier("table name"));
    assert!(!is_safe_identifier("table;drop"));
    assert!(!is_safe_identifier("table\"name"));
    assert!(!is_safe_identifier(&"x".repeat(129)));
}

#[test]
fn test_distance_metric_default() {
    assert_eq!(DistanceMetric::default(), DistanceMetric::Cosine);
}

#[test]
fn test_config_serde_roundtrip() {
    let config = test_config();
    let json = serde_json::to_string(&config).unwrap();
    let restored: PgvectorConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.table, "documents");
    assert_eq!(restored.distance_metric, DistanceMetric::Cosine);
    assert_eq!(restored.dimensions, Some(384));
}
