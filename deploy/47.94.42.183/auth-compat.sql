-- The stock GoTrue image does not include PonyNotes' password status column.
ALTER TABLE auth.users ADD COLUMN IF NOT EXISTS password_is_set boolean DEFAULT false;

CREATE OR REPLACE FUNCTION auth.ponynotes_password_status() RETURNS trigger AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        NEW.password_is_set := COALESCE(NEW.encrypted_password, '') <> '';
    ELSIF NEW.encrypted_password IS DISTINCT FROM OLD.encrypted_password THEN
        NEW.password_is_set := COALESCE(NEW.encrypted_password, '') <> '';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS ponynotes_password_status ON auth.users;
CREATE TRIGGER ponynotes_password_status BEFORE INSERT OR UPDATE OF encrypted_password ON auth.users
FOR EACH ROW EXECUTE FUNCTION auth.ponynotes_password_status();

UPDATE auth.users SET password_is_set = (COALESCE(encrypted_password, '') <> '')
WHERE password_is_set IS DISTINCT FROM (COALESCE(encrypted_password, '') <> '');
