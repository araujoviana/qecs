//! Elastic Cloud Server - server lifecycle models (calls land in branch 2).
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct ServersResp {
    pub servers: Vec<Server>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_servers() {
        let j = r#"{"servers":[{"id":"i-1","name":"qecs-gpu-ab12","status":"ACTIVE"}]}"#;
        let r: ServersResp = serde_json::from_str(j).unwrap();
        assert_eq!(r.servers[0].status, "ACTIVE");
    }
}
