-- 支持手机号用户邀请加入工作空间
-- 修改触发器函数，使其同时支持邮箱和手机号查找用户

-- 重新创建触发器函数，支持邮箱或手机号
CREATE OR REPLACE FUNCTION add_to_af_workspace_member()
RETURNS TRIGGER AS $$
DECLARE
  invitee_uid BIGINT;
BEGIN
  IF NEW.status = 1 THEN
    -- 首先尝试通过邮箱查找用户
    SELECT uid INTO invitee_uid FROM af_user WHERE email = NEW.invitee_email;
    
    -- 如果邮箱没找到，尝试通过手机号查找（invitee_email 字段可能存储的是手机号）
    IF invitee_uid IS NULL THEN
      SELECT uid INTO invitee_uid FROM af_user 
      WHERE phone = NEW.invitee_email 
         OR phone = CONCAT('+86', NEW.invitee_email)
         OR phone = LTRIM(NEW.invitee_email, '+');
    END IF;
    
    -- 如果找到了用户，添加到工作空间成员
    IF invitee_uid IS NOT NULL THEN
      -- workspace permission
      INSERT INTO af_workspace_member (workspace_id, uid, role_id)
      VALUES (
        NEW.workspace_id,
        invitee_uid,
        NEW.role_id
      )
      ON CONFLICT (workspace_id, uid) DO NOTHING;

      -- collab permission
      INSERT INTO af_collab_member (uid, oid, permission_id)
      VALUES (
        invitee_uid,
        NEW.workspace_id,
        (SELECT permission_id
         FROM public.af_role_permissions
         WHERE public.af_role_permissions.role_id = NEW.role_id)
      )
      ON CONFLICT (uid, oid)
      DO UPDATE
        SET permission_id = excluded.permission_id;
    END IF;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- 注释说明
COMMENT ON FUNCTION add_to_af_workspace_member() IS '当工作空间邀请状态变为已接受时，自动添加成员到工作空间。支持通过邮箱或手机号查找被邀请的用户。';


