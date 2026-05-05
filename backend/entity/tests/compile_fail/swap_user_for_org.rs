use entity::ids::{OrganizationId, UserId};
use uuid::Uuid;

fn user_is_org_member(_org: OrganizationId, _user: UserId) -> bool {
    true
}

fn main() {
    let u = Uuid::now_v7();
    let org_id = OrganizationId::from(u);
    let user_id = UserId::from(u);
    // Swapped — must fail to compile.
    let _ = user_is_org_member(user_id, org_id);
}
