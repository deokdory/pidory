use percent_encoding::percent_decode_str;
use url::Url;

#[derive(Debug, Clone, PartialEq)]
pub struct PgUrlParts {
    pub user: String,
    pub password: Option<String>,
    pub host: String,
    pub port: u16,
    pub dbname: String,
}

pub fn parse_pg_url(s: &str) -> Result<PgUrlParts, super::Error> {
    let parsed = Url::parse(s)
        .map_err(|_| super::Error::BackupFailed("DATABASE_URL 파싱 실패".into()))?;

    match parsed.scheme() {
        "postgres" | "postgresql" => {}
        _ => return Err(super::Error::BackupFailed("DATABASE_URL 파싱 실패".into())),
    }

    let raw_user = parsed.username();
    if raw_user.is_empty() {
        return Err(super::Error::BackupFailed("DATABASE_URL 파싱 실패".into()));
    }
    let user = percent_decode_str(raw_user)
        .decode_utf8_lossy()
        .into_owned();

    let password = parsed.password().map(|p| {
        percent_decode_str(p).decode_utf8_lossy().into_owned()
    });

    let host = parsed
        .host_str()
        .ok_or_else(|| super::Error::BackupFailed("DATABASE_URL 파싱 실패".into()))?
        .to_string();

    let port = parsed.port().unwrap_or(5432);

    let raw_path = parsed.path();
    let dbname = raw_path.trim_start_matches('/');
    if dbname.is_empty() {
        return Err(super::Error::BackupFailed("DATABASE_URL 파싱 실패".into()));
    }
    let dbname = dbname.to_string();

    Ok(PgUrlParts {
        user,
        password,
        host,
        port,
        dbname,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_url() {
        let parts = parse_pg_url("postgres://pidory:secret@localhost/pidory").unwrap();
        assert_eq!(parts.user, "pidory");
        assert_eq!(parts.password.as_deref(), Some("secret"));
        assert_eq!(parts.host, "localhost");
        assert_eq!(parts.port, 5432);
        assert_eq!(parts.dbname, "pidory");
    }

    #[test]
    fn parses_percent_encoded_password() {
        // pa%40ss%23word → pa@ss#word
        let parts = parse_pg_url("postgres://pidory:pa%40ss%23word@localhost:5433/pidory").unwrap();
        assert_eq!(parts.user, "pidory");
        assert_eq!(parts.password.as_deref(), Some("pa@ss#word"));
        assert_eq!(parts.port, 5433);
        assert_eq!(parts.dbname, "pidory");
    }

    #[test]
    fn parses_url_without_password() {
        let parts = parse_pg_url("postgresql://pidory@localhost/pidory").unwrap();
        assert_eq!(parts.user, "pidory");
        assert_eq!(parts.password, None);
        assert_eq!(parts.host, "localhost");
        assert_eq!(parts.port, 5432);
        assert_eq!(parts.dbname, "pidory");
    }

    #[test]
    fn parses_custom_host_port_dbname() {
        let parts = parse_pg_url("postgres://pidory:secret@db.example.com:6543/mydb").unwrap();
        assert_eq!(parts.host, "db.example.com");
        assert_eq!(parts.port, 6543);
        assert_eq!(parts.dbname, "mydb");
    }

    #[test]
    fn rejects_invalid_or_wrong_scheme() {
        // 잘못된 URL
        assert!(parse_pg_url("not-a-url").is_err());
        // 잘못된 scheme
        assert!(parse_pg_url("http://pidory:secret@localhost/pidory").is_err());
        // user 없음
        assert!(parse_pg_url("postgres://:secret@localhost/pidory").is_err());
        // dbname 없음
        assert!(parse_pg_url("postgres://pidory:secret@localhost/").is_err());
    }
}
