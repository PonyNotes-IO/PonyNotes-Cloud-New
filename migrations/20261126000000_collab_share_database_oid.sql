-- Linked views share one database collab. Keep the grant source on each invitation.
ALTER TABLE af_collab_member_invite ADD COLUMN database_oid UUID;
