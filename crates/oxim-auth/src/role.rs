use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Something a user may be allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// See the dashboard, channel states and queue statistics.
    ViewDashboard,
    /// List messages and read their content with patient data masked.
    ViewMessages,
    /// Read message content without masking.
    ViewUnmasked,
    /// Reprocess messages and requeue deliveries.
    RepairMessages,
    /// Delete messages permanently (erasure requests).
    EraseMessages,
    /// Read channel configuration.
    ViewChannels,
    /// Create, change and delete channel configuration.
    EditChannels,
    /// Deploy, undeploy and redeploy channels.
    DeployChannels,
    /// Read code tables.
    ViewTables,
    /// Change code tables.
    EditTables,
    /// Manage users and their sessions.
    ManageUsers,
    /// Manage API tokens.
    ManageTokens,
    /// Read the audit trail.
    ViewAudit,
    /// Read system information and metrics.
    ViewSystem,
    /// Create and download backups and switch maintenance mode.
    ManageSystem,
}

impl Permission {
    /// Every permission.
    pub const ALL: [Self; 15] = [
        Self::ViewDashboard,
        Self::ViewMessages,
        Self::ViewUnmasked,
        Self::RepairMessages,
        Self::EraseMessages,
        Self::ViewChannels,
        Self::EditChannels,
        Self::DeployChannels,
        Self::ViewTables,
        Self::EditTables,
        Self::ManageUsers,
        Self::ManageTokens,
        Self::ViewAudit,
        Self::ViewSystem,
        Self::ManageSystem,
    ];
}

/// A role: a fixed set of permissions.
///
/// Roles apply to every channel. Scoping a role grant to a list of
/// channels is planned; until then, use separate OXIM instances where
/// channel-level separation is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Everything, including users, tokens, erasure and the audit trail.
    Admin,
    /// Day-to-day operation: unmasked messages, repairs, channel and code
    /// table changes, deployment.
    Operator,
    /// Read-only access with patient-identifying values masked.
    Viewer,
}

impl Role {
    /// Every role, most privileged first.
    pub const ALL: [Self; 3] = [Self::Admin, Self::Operator, Self::Viewer];

    /// The role's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Operator => "operator",
            Self::Viewer => "viewer",
        }
    }

    /// Whether the role grants `permission`.
    pub fn allows(self, permission: Permission) -> bool {
        use Permission::*;
        match self {
            Self::Admin => true,
            Self::Operator => !matches!(
                permission,
                EraseMessages | ManageUsers | ManageTokens | ViewAudit | ManageSystem
            ),
            Self::Viewer => matches!(
                permission,
                ViewDashboard | ViewMessages | ViewChannels | ViewTables | ViewSystem
            ),
        }
    }

    /// The permissions the role grants.
    pub fn permissions(self) -> Vec<Permission> {
        Permission::ALL
            .into_iter()
            .filter(|p| self.allows(*p))
            .collect()
    }

    /// Whether this role has at least every permission of `other`.
    pub fn covers(self, other: Role) -> bool {
        other.permissions().iter().all(|p| self.allows(*p))
    }
}

/// Returned when a role name is unknown.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown role {0:?}; use admin, operator or viewer")]
pub struct UnknownRole(pub String);

impl FromStr for Role {
    type Err = UnknownRole;

    fn from_str(s: &str) -> Result<Self, UnknownRole> {
        Self::ALL
            .into_iter()
            .find(|role| role.as_str() == s)
            .ok_or_else(|| UnknownRole(s.to_owned()))
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_grant_the_documented_permissions() {
        assert!(Role::Admin.allows(Permission::EraseMessages));
        assert!(Role::Operator.allows(Permission::ViewUnmasked));
        assert!(Role::Operator.allows(Permission::DeployChannels));
        assert!(!Role::Operator.allows(Permission::ManageUsers));
        assert!(!Role::Operator.allows(Permission::ViewAudit));
        assert!(!Role::Operator.allows(Permission::ManageSystem));
        assert!(Role::Admin.allows(Permission::ManageSystem));
        assert!(Role::Viewer.allows(Permission::ViewMessages));
        assert!(!Role::Viewer.allows(Permission::ViewUnmasked));
        assert!(!Role::Viewer.allows(Permission::RepairMessages));
        assert!(Role::Admin.covers(Role::Operator));
        assert!(Role::Operator.covers(Role::Viewer));
        assert!(!Role::Viewer.covers(Role::Operator));
    }

    #[test]
    fn role_names_round_trip() {
        for role in Role::ALL {
            assert_eq!(role.as_str().parse::<Role>().unwrap(), role);
        }
        assert!("root".parse::<Role>().is_err());
    }
}
