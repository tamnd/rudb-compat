-- fires
-- NOT LIKE on a NULL note is NULL, not true, so movie_info rows 1, 4 and 5 must not survive it.
SELECT MIN(t.title) AS movie_title, MIN(mi.info) AS info FROM title AS t, movie_info AS mi
WHERE t.id = mi.movie_id AND mi.note NOT LIKE '%(TV)%';
