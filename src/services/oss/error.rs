// Copyright 2022 Datafuse Labs.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use bytes::Buf;
use http::Response;
use http::StatusCode;
use quick_xml::de;
use serde::Deserialize;

use crate::raw::*;
use crate::Error;
use crate::ErrorKind;
use crate::Result;

/// OssError is the error returned by oss service.
#[derive(Default, Debug, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct OssError {
    code: String,
    message: String,
    request_id: String,
    host_id: String,
}

/// Parse error respons into Error.
pub async fn parse_error(resp: Response<IncomingAsyncBody>) -> Result<Error> {
    let (parts, body) = resp.into_parts();
    let bs = body.bytes().await?;

    let (kind, retryable) = match parts.status {
        StatusCode::NOT_FOUND => (ErrorKind::ObjectNotFound, false),
        StatusCode::FORBIDDEN => (ErrorKind::ObjectPermissionDenied, false),
        StatusCode::INTERNAL_SERVER_ERROR
        | StatusCode::BAD_GATEWAY
        | StatusCode::SERVICE_UNAVAILABLE
        | StatusCode::GATEWAY_TIMEOUT => (ErrorKind::Unexpected, true),
        _ => (ErrorKind::Unexpected, false),
    };

    let message = match de::from_reader::<_, OssError>(bs.clone().reader()) {
        Ok(oss_err) => format!("{:?}", oss_err),
        Err(_) => String::from_utf8_lossy(&bs).into_owned(),
    };

    let mut err = Error::new(kind, &message).with_context("response", format!("{:?}", parts));

    if retryable {
        err = err.set_temporary();
    }

    Ok(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Error response example is from https://www.alibabacloud.com/help/en/object-storage-service/latest/error-responses
    #[test]
    fn test_parse_error() {
        let bs = bytes::Bytes::from(
            r#"
<?xml version="1.0" ?>
<Error xmlns="http://doc.oss-cn-hangzhou.aliyuncs.com">
    <Code>
        AccessDenied
    </Code>
    <Message>
        Query-string authentication requires the Signature, Expires and OSSAccessKeyId parameters
    </Message>
    <RequestId>
        1D842BC54255****
    </RequestId>
    <HostId>
        oss-cn-hangzhou.aliyuncs.com
    </HostId>
</Error>
"#,
        );

        let out: OssError = de::from_reader(bs.reader()).expect("must success");
        println!("{:?}", out);

        assert_eq!(out.code, "AccessDenied");
        assert_eq!(out.message, "Query-string authentication requires the Signature, Expires and OSSAccessKeyId parameters");
        assert_eq!(out.request_id, "1D842BC54255****");
        assert_eq!(out.host_id, "oss-cn-hangzhou.aliyuncs.com");
    }
}
