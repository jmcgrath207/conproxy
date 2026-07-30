/// Build a gRPC request with optional API key metadata.
pub(crate) fn make_request<T>(inner: T, api_key: &Option<String>) -> tonic::Request<T> {
    let mut request = tonic::Request::new(inner);
    if let Some(ref key) = api_key {
        if let Ok(value) = key.parse() {
            request.metadata_mut().insert("x-api-key", value);
        }
    }
    request
}
