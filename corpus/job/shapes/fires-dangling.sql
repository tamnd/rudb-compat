-- fires
-- movie_keyword row 7 points at movie 99, which is not in title. A reducer that trusted a foreign key would keep it.
SELECT MAX(mk.movie_id) AS movie, MAX(k.keyword) AS keyword FROM movie_keyword AS mk, keyword AS k, title AS t
WHERE mk.keyword_id = k.id AND mk.movie_id = t.id AND k.keyword = 'murder';
