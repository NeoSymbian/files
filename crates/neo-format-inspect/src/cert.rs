// SPDX-License-Identifier: Apache-2.0
//! The certificate viewer: X.509 certificates (PEM, a PEM bundle, or DER, one or several back to back as in a SIS
//! signature) parsed by `x509-cert`. View only: nothing here checks a signature, so "self-signed" means only that the
//! issuer and the subject are the same name, and nothing here says a certificate is trusted.

use crate::{Error, Result};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use der::Decode;
use der::asn1::UintRef;
use sha2::{Digest, Sha256};
use x509_cert::Certificate;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{BasicConstraints, SubjectAltName};

/// At most this many certificates from one file (a system bundle has ~150).
const MAX_CERTS: usize = 1000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Name {
    pub cn: Option<String>,
    pub o: Option<String>,
    pub ou: Option<String>,
    pub c: Option<String>,
}

/// UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Time {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Time {
    pub fn unix(&self) -> i64 {
        crate::unix_time(
            self.year as i64,
            self.month as u32,
            self.day as u32,
            self.hour as u32,
            self.minute as u32,
            self.second as u32,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cert {
    pub subject: Name,
    pub issuer: Name,
    pub not_before: Time,
    pub not_after: Time,
    /// Hex bytes joined by ':'.
    pub serial: String,
    /// "EC P-256", "RSA 2048", "Ed25519", "DSA", else the algorithm's OID.
    pub key: String,
    /// "SHA-256 with ECDSA", "SHA-256 with RSA", …, else the OID.
    pub signature: String,
    /// DNS names, e-mail addresses, URIs and IP addresses.
    pub san: Vec<String>,
    /// basicConstraints says CA.
    pub ca: bool,
    /// The issuer is the subject. The signature is NOT verified.
    pub self_signed: bool,
    /// SHA-256 of the DER: the fingerprint.
    pub sha256: [u8; 32],
}

/// Every certificate of a PEM file (other blocks, such as keys, skipped) or of back-to-back DER.
pub fn parse(data: &[u8]) -> Result<Vec<Cert>> {
    let mut out = Vec::new();
    if data.first() == Some(&0x30) {
        let mut rest = data;
        while rest.first() == Some(&0x30) && out.len() < MAX_CERTS {
            match Certificate::from_der_partial(rest) {
                Ok((c, tail)) => {
                    let der = &rest[..rest.len() - tail.len()];
                    out.push(convert(&c, der));
                    rest = tail;
                }
                Err(e) if out.is_empty() => return Err(der_error(e)),
                // trailing junk after good certificates: keep those
                Err(_) => break,
            }
        }
    } else {
        const BEGIN: &[u8] = b"-----BEGIN CERTIFICATE-----";
        const END: &[u8] = b"-----END CERTIFICATE-----";
        let mut at = 0;
        let mut blocks = 0;
        while let Some(b) = find(&data[at..], BEGIN).map(|i| at + i) {
            let Some(e) = find(&data[b..], END).map(|i| b + i + END.len()) else {
                break;
            };
            at = e;
            blocks += 1;
            if out.len() >= MAX_CERTS {
                break;
            }
            let Ok((_, der)) = pem_rfc7468::decode_vec(&data[b..e]) else {
                continue;
            };
            if let Ok(c) = Certificate::from_der(&der) {
                out.push(convert(&c, &der));
            }
        }
        if blocks > 0 && out.is_empty() {
            return Err(Error::Bad("The certificate is damaged"));
        }
    }
    if out.is_empty() {
        return Err(Error::Bad("Not a certificate"));
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Validity {
    NotYet,
    Valid { days_left: i64 },
    Expired { days_ago: i64 },
}

pub fn validity(c: &Cert, now_unix: i64) -> Validity {
    if now_unix < c.not_before.unix() {
        Validity::NotYet
    } else if now_unix > c.not_after.unix() {
        Validity::Expired {
            days_ago: (now_unix - c.not_after.unix()) / 86_400,
        }
    } else {
        Validity::Valid {
            days_left: (c.not_after.unix() - now_unix) / 86_400,
        }
    }
}

/// The fingerprint on two lines of 16 bytes, the first ending with ':'.
pub fn fingerprint_lines(sha: &[u8; 32]) -> [String; 2] {
    let line = |b: &[u8]| {
        b.iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    };
    [line(&sha[..16]) + ":", line(&sha[16..])]
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn der_error(e: der::Error) -> Error {
    match e.kind() {
        der::ErrorKind::Incomplete { .. } => Error::Truncated,
        _ => Error::Bad("The certificate is damaged"),
    }
}

fn convert(c: &Certificate, der: &[u8]) -> Cert {
    let tbs = c.tbs_certificate();
    let subject = name(tbs.subject());
    let issuer = name(tbs.issuer());
    let mut san = Vec::new();
    if let Ok(Some((_, SubjectAltName(names)))) = tbs.get_extension::<SubjectAltName>() {
        for n in names {
            match n {
                GeneralName::DnsName(s)
                | GeneralName::Rfc822Name(s)
                | GeneralName::UniformResourceIdentifier(s) => san.push(s.to_string()),
                GeneralName::IpAddress(ip) => san.push(ip_text(ip.as_bytes())),
                _ => {}
            }
        }
    }
    let ca = matches!(tbs.get_extension::<BasicConstraints>(), Ok(Some((_, b))) if b.ca);
    Cert {
        self_signed: tbs.subject() == tbs.issuer(),
        subject,
        issuer,
        not_before: time(tbs.validity().not_before),
        not_after: time(tbs.validity().not_after),
        serial: tbs
            .serial_number()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":"),
        key: key(tbs.subject_public_key_info()),
        signature: signature(&c.signature_algorithm().oid.to_string()),
        san,
        ca,
        sha256: Sha256::digest(der).into(),
    }
}

fn name(n: &x509_cert::name::Name) -> Name {
    Name {
        cn: n.common_name().ok().flatten().map(String::from),
        o: n.organization().ok().flatten().map(String::from),
        ou: n.organization_unit().ok().flatten().map(String::from),
        c: n.country().ok().flatten().map(|c| c.as_str().to_string()),
    }
}

fn time(t: x509_cert::time::Time) -> Time {
    let d = t.to_date_time();
    Time {
        year: d.year(),
        month: d.month(),
        day: d.day(),
        hour: d.hour(),
        minute: d.minutes(),
        second: d.seconds(),
    }
}

fn ip_text(b: &[u8]) -> String {
    match b.len() {
        4 => format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3]),
        _ => b
            .chunks(2)
            .map(|p| format!("{:x}", u16::from_be_bytes([p[0], *p.get(1).unwrap_or(&0)])))
            .collect::<Vec<_>>()
            .join(":"),
    }
}

fn key(spki: &x509_cert::spki::SubjectPublicKeyInfoOwned) -> String {
    let alg = spki.algorithm.oid.to_string();
    match alg.as_str() {
        "1.2.840.10045.2.1" => {
            let curve = spki
                .algorithm
                .parameters
                .as_ref()
                .and_then(|p| p.decode_as::<der::asn1::ObjectIdentifier>().ok())
                .map(|o| o.to_string());
            match curve.as_deref() {
                Some("1.2.840.10045.3.1.7") => "EC P-256".into(),
                Some("1.3.132.0.34") => "EC P-384".into(),
                Some("1.3.132.0.35") => "EC P-521".into(),
                Some(o) => format!("EC {o}"),
                None => "EC".into(),
            }
        }
        "1.2.840.113549.1.1.1" => match rsa_bits(spki.subject_public_key.raw_bytes()) {
            Some(bits) => format!("RSA {bits}"),
            None => "RSA".into(),
        },
        "1.3.101.112" => "Ed25519".into(),
        "1.3.101.113" => "Ed448".into(),
        "1.2.840.10040.4.1" => "DSA".into(),
        _ => alg,
    }
}

/// The modulus length of an RSAPublicKey (SEQUENCE { modulus INTEGER, publicExponent INTEGER }).
fn rsa_bits(der: &[u8]) -> Option<usize> {
    let mut r = der::SliceReader::new(der).ok()?;
    let n = der::Reader::sequence(&mut r, |r| {
        let n = UintRef::decode(r)?;
        let _e = UintRef::decode(r)?;
        Ok::<_, der::Error>(n.as_bytes().to_vec())
    })
    .ok()?;
    let first = *n.first()?;
    Some(n.len() * 8 - first.leading_zeros() as usize)
}

fn signature(oid: &str) -> String {
    let name = match oid {
        "1.2.840.113549.1.1.4" => "MD5 with RSA",
        "1.2.840.113549.1.1.5" => "SHA-1 with RSA",
        "1.2.840.113549.1.1.10" => "RSA-PSS",
        "1.2.840.113549.1.1.11" => "SHA-256 with RSA",
        "1.2.840.113549.1.1.12" => "SHA-384 with RSA",
        "1.2.840.113549.1.1.13" => "SHA-512 with RSA",
        "1.2.840.10045.4.1" => "SHA-1 with ECDSA",
        "1.2.840.10045.4.3.2" => "SHA-256 with ECDSA",
        "1.2.840.10045.4.3.3" => "SHA-384 with ECDSA",
        "1.2.840.10045.4.3.4" => "SHA-512 with ECDSA",
        "1.2.840.10040.4.3" => "SHA-1 with DSA",
        "2.16.840.1.101.3.4.3.2" => "SHA-256 with DSA",
        "1.3.101.112" => "Ed25519",
        "1.3.101.113" => "Ed448",
        _ => return oid.into(),
    };
    name.into()
}
