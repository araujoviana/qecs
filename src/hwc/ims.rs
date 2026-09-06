//! Image Management Service - image lookup (calls land in branch 2).
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Image {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct ImagesResp {
    pub images: Vec<Image>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_images() {
        let j = r#"{"images":[{"id":"abc","name":"Ubuntu 22.04 server 64bit"}]}"#;
        let r: ImagesResp = serde_json::from_str(j).unwrap();
        assert_eq!(r.images[0].name, "Ubuntu 22.04 server 64bit");
    }
}
