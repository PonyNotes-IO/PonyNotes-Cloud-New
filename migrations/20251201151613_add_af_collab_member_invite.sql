-- Add migration script here
create table af_collab_member_invite (
                                         oid           text                    not null,
                                         send_uid      bigint                  not null
                                             references public.af_user (uid)
                                                 on delete cascade,
                                         received_uid  bigint                  not null
                                             references public.af_user (uid)
                                                 on delete cascade,
                                         created_at    timestamp with time zone default CURRENT_TIMESTAMP not null, -- 修正：缺少逗号

    -- 修正：使用 CONSTRAINT 关键字并定义约束名称（推荐）
                                         constraint af_collab_member_invite_pk primary key (oid, send_uid, received_uid)
);

comment on table af_collab_member_invite is '成员邀请记录';

comment on column af_collab_member_invite.send_uid is '邀请者的用户ID';
comment on column af_collab_member_invite.received_uid is '被邀请者的用户ID';
comment on column af_collab_member_invite.oid is '关联对象的Object ID';
comment on column af_collab_member_invite.created_at is '邀请创建时间';