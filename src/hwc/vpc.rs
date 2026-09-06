//! Virtual Private Cloud - network models (calls land in branch 2).
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Vpc {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct VpcsResp {
    pub vpcs: Vec<Vpc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vpcs() {
        let j = r#"{"vpcs":[{"id":"v-1","name":"qecs-vpc"}]}"#;
        let r: VpcsResp = serde_json::from_str(j).unwrap();
        assert_eq!(r.vpcs[0].name, "qecs-vpc");
    }
}
