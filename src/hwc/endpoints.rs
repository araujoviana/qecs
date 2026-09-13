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
