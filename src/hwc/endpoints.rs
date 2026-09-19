//! region + service -> API host.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Ecs,
    Ims,
    Vpc,
    Iam,
    Obs,
}

impl Service {
    fn prefix(self) -> &'static str {
        match self {
            Service::Ecs => "ecs",
            Service::Ims => "ims",
            Service::Vpc => "vpc",
            Service::Iam => "iam",
            Service::Obs => "obs",
        }
    }
}

pub fn endpoint_host(service: Service, region: &str) -> String {
    format!("{}.{region}.myhuaweicloud.com", service.prefix())
}

pub fn endpoint_url(service: Service, region: &str) -> String {
    if let Ok(override_url) = std::env::var("QECS_TEST_ENDPOINT") {
        let trimmed = override_url.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    format!("https://{}", endpoint_host(service, region))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        assert_eq!(
            endpoint_host(Service::Ecs, "ap-southeast-3"),
            "ecs.ap-southeast-3.myhuaweicloud.com"
        );
        assert_eq!(
            endpoint_host(Service::Ims, "sa-brazil-1"),
            "ims.sa-brazil-1.myhuaweicloud.com"
        );
        assert_eq!(
            endpoint_host(Service::Obs, "ap-southeast-3"),
            "obs.ap-southeast-3.myhuaweicloud.com"
        );
    }
}
