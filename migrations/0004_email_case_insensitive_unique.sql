-- Emails are compared case-insensitively by the application; enforce it in the database too.
CREATE UNIQUE INDEX users_email_lower_key ON users (lower(email));
