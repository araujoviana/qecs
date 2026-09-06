//! region + service -> API host.

#[derive(Debug, Clone, Copy)]
pub enum Service {
    Ecs,
    Ims,
    Vpc,
    Iam,
}

impl Service {
    fn prefix(self) -> &'static str {
        match self {
            Service::Ecs => "ecs",
            Service::Ims => "ims",
            Service::Vpc => "vpc",
            Service::Iam => "iam",
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
    }
}
