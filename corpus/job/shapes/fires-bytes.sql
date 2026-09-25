-- fires
-- The empty string is the least title, and the emoji is the greatest in UTF-8 byte order but not in UTF-16 order, where the fullwidth z is.
SELECT MIN(t.title) AS least, MAX(t.title) AS greatest FROM title AS t, movie_keyword AS mk
WHERE t.id = mk.movie_id;
