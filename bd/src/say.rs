//! `bd say-ipa`: read IPA aloud with Amazon Polly, the way ipa-reader.com does
//! (anonymous Cognito identity -> SigV4-presigned Polly URL -> afplay).
//! HTTP goes through the system `curl`, so no TLS stack is linked into `bd`.
use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::process::{Command, Stdio};

const REGION: &str = "us-west-2";
const POOL_ID: &str = "us-west-2:42521701-f77a-4555-8b1c-e160ad0210da";
const REFERER: &str = "https://ipa-reader.com/";
const HOST: &str = "polly.us-west-2.amazonaws.com";

fn hmac(key: &[u8], msg: &str) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).unwrap();
    m.update(msg.as_bytes());
    m.finalize().into_bytes().to_vec()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k = hmac(format!("AWS4{secret}").as_bytes(), date);
    let k = hmac(&k, region);
    let k = hmac(&k, service);
    hmac(&k, "aws4_request")
}

fn uri_encode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// (yyyymmdd, yyyymmddThhmmssZ) for a unix time.
fn amz_time(secs: u64) -> (String, String) {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // civil-from-days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let date = format!("{y:04}{m:02}{d:02}");
    let full = format!("{date}T{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60);
    (date, full)
}

/// SigV4 query-string presigned GET for Polly SynthesizeSpeech.
fn presign(ak: &str, sk: &str, token: &str, ssml: &str, voice: &str, secs: u64) -> String {
    let (date, stamp) = amz_time(secs);
    let scope = format!("{date}/{REGION}/polly/aws4_request");
    let mut q: Vec<(String, String)> = vec![
        ("OutputFormat".into(), "mp3".into()),
        ("SampleRate".into(), "16000".into()),
        ("Text".into(), ssml.into()),
        ("TextType".into(), "ssml".into()),
        ("VoiceId".into(), voice.into()),
        ("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into()),
        ("X-Amz-Credential".into(), format!("{ak}/{scope}")),
        ("X-Amz-Date".into(), stamp.clone()),
        ("X-Amz-Expires".into(), "3600".into()),
        ("X-Amz-Security-Token".into(), token.into()),
        ("X-Amz-SignedHeaders".into(), "host".into()),
    ];
    q.sort();
    let query: String = q.iter().map(|(k, v)| format!("{}={}", uri_encode(k), uri_encode(v))).collect::<Vec<_>>().join("&");
    let empty = hex(&Sha256::digest(b""));
    let canonical = format!("GET\n/v1/speech\n{query}\nhost:{HOST}\n\nhost\n{empty}");
    let to_sign = format!("AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}", hex(&Sha256::digest(canonical.as_bytes())));
    let sig = hex(&hmac(&signing_key(sk, &date, REGION, "polly"), &to_sign));
    format!("https://{HOST}/v1/speech?{query}&X-Amz-Signature={sig}")
}

fn curl_json(target: &str, body: &Value) -> Result<Value> {
    let out = Command::new("/usr/bin/curl")
        .args(["-sS", "--fail-with-body", "--max-time", "15", "-X", "POST"])
        .args(["-H", "Content-Type: application/x-amz-json-1.1"])
        .args(["-H", &format!("X-Amz-Target: AWSCognitoIdentityService.{target}")])
        .args(["-d", &body.to_string()])
        .arg(format!("https://cognito-identity.{REGION}.amazonaws.com/"))
        .stderr(Stdio::inherit())
        .output()
        .context("running curl")?;
    if !out.status.success() {
        bail!("cognito {target} failed: {}", String::from_utf8_lossy(&out.stdout));
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

pub fn say_ipa(ipa: &str, voice: &str, out: Option<&str>, play: bool) -> Result<()> {
    let tmp_out = std::env::temp_dir().join(format!("bd-ipa-{}.mp3", std::process::id())).to_string_lossy().into_owned();
    let out = out.unwrap_or(&tmp_out);
    let id = curl_json("GetId", &json!({"IdentityPoolId": POOL_ID}))?;
    let id = id["IdentityId"].as_str().context("no IdentityId")?;
    let c = curl_json("GetCredentialsForIdentity", &json!({"IdentityId": id}))?;
    let c = &c["Credentials"];
    let (ak, sk, tok) = (
        c["AccessKeyId"].as_str().context("no AccessKeyId")?,
        c["SecretKey"].as_str().context("no SecretKey")?,
        c["SessionToken"].as_str().context("no SessionToken")?,
    );
    let esc = ipa.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('\'', "&apos;").replace('"', "&quot;");
    let ssml = format!("<speak><phoneme alphabet='ipa' ph='{esc}'></phoneme></speak>");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let url = presign(ak, sk, tok, &ssml, voice, now);
    let st = Command::new("/usr/bin/curl")
        .args(["-sS", "--fail", "--max-time", "20", "-H", &format!("Referer: {REFERER}"), "-o", out])
        .arg(&url)
        .status()
        .context("running curl")?;
    if !st.success() {
        bail!("downloading audio failed");
    }
    if play {
        let r = Command::new("afplay").arg(out).status().context("afplay");
        let _ = std::fs::remove_file(out);
        r?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_key_matches_aws_docs() {
        // Example from the AWS SigV4 documentation (iam / us-east-1 / 20150830).
        let k = signing_key("wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "20150830", "us-east-1", "iam");
        assert_eq!(hex(&k), "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9");
    }

    #[test]
    fn time_format() {
        assert_eq!(amz_time(1440892800).1, "20150830T000000Z");
        assert_eq!(amz_time(1709210096).1, "20240229T123456Z");
    }

    #[test]
    fn encoding() {
        assert_eq!(uri_encode("<a b='é'>"), "%3Ca%20b%3D%27%C3%A9%27%3E");
    }
}
