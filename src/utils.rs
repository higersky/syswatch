use actix_web::http::StatusCode;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use users::os::unix::UserExt;
use users::{uid_t, User};

fn get_users() -> (Vec<User>, Vec<User>) {
    let linux_logindefs = fs::read_to_string("/etc/login.defs")
        .expect("Cannot find /etc/login.defs. Your linux may be corrputed.");
    let linux_logindefs = whitespace_conf::parse(&linux_logindefs);
    let uid_min = linux_logindefs
        .get("UID_MIN")
        .expect("Cannot get UID_MIN")
        .parse::<u32>()
        .unwrap();
    let uid_max = linux_logindefs
        .get("UID_MAX")
        .expect("Cannot get UID_MAX")
        .parse::<u32>()
        .unwrap();

    let nologin_path = Path::new("/sbin/nologin");

    // Critical section
    static MUTEX: Mutex<bool> = Mutex::new(false);
    {
        let mut _data = MUTEX.lock().expect("Failed to get users");

        let users = unsafe { users::all_users() };
        users.into_iter().partition(|user| {
            (uid_min..uid_max).contains(&user.uid()) && user.shell() != nologin_path
        })
    }
}

pub fn get_users_map() -> (HashMap<uid_t, User>, HashMap<uid_t, User>) {
    let mut known_user_map: HashMap<uid_t, User> = HashMap::new();
    let mut blocked_user_map = HashMap::new();

    let (known_users, blocked_users) = get_users();
    for user in known_users {
        known_user_map.entry(user.uid()).or_insert(user);
    }
    for user in blocked_users {
        blocked_user_map.entry(user.uid()).or_insert(user);
    }

    (known_user_map, blocked_user_map)
}

pub trait IntoHttpError<T> {
    fn http_error(
        self,
        message: &str,
        status_code: StatusCode,
    ) -> core::result::Result<T, actix_web::Error>;

    fn http_internal_error(self, message: &str) -> core::result::Result<T, actix_web::Error>
    where
        Self: std::marker::Sized,
    {
        self.http_error(message, StatusCode::INTERNAL_SERVER_ERROR)
    }
}

impl<T, E: std::fmt::Debug> IntoHttpError<T> for core::result::Result<T, E> {
    fn http_error(
        self,
        message: &str,
        status_code: StatusCode,
    ) -> core::result::Result<T, actix_web::Error> {
        match self {
            Ok(val) => Ok(val),
            Err(err) => {
                eprintln!("http_error: {:?}", err);
                Err(actix_web::error::InternalError::new(message.to_string(), status_code).into())
            }
        }
    }
}
